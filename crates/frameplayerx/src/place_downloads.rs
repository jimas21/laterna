//! Optional places the person can download from Settings > Places.
//!
//! The list of packs is built into the app (`packaging/places-optional.lock`,
//! so each release vouches for every pack's SHA-256 and size) and is empty
//! until a pack is published. This module holds the state of the one
//! download that may run at a time, on a worker thread, and the installed
//! packs; `fp_updater::places` does the work and `fp_render::env::pack`
//! decides whether a downloaded pack is acceptable.
//!
//! The person's own pack sources work the same way with three differences:
//! the index is fetched from the address they added (so nothing vouches for
//! it but them: Settings says "Not from Laterna"), every download is
//! https-only and asks first, and a pack installs into their `environments`
//! folder (see `fp_render::env::user`) under its own name, so it appears as
//! one of their places and can never replace a built-in one. Removing a
//! source forgets what it offered, not what was installed from it.
//!
//! `LATERNA_PLACES_CATALOG=<file>` replaces the built-in list with a file of
//! the same format, and allows plain http to this machine, but only when
//! every entry in the file is on this machine (otherwise every download is
//! https only). It is for tests and the desktop preview; the hashes are
//! still checked.

use fp_core::sync::lock;
use fp_updater::places::{self, CatalogEntry};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

/// The environment variable that replaces the built-in catalog (tests and
/// the preview only).
pub const CATALOG_ENV: &str = "LATERNA_PLACES_CATALOG";

const BUILT_IN: &str = include_str!("../../../packaging/places-optional.lock");

/// The built-in catalog, parsed. A bad file is a mistake in the repository
/// (a test refuses it); at run time it is logged and treated as empty.
fn built_in() -> Vec<CatalogEntry> {
    places::parse_catalog(BUILT_IN, false).unwrap_or_else(|e| {
        log::warn!("the built-in place catalog is not usable: {e}");
        Vec::new()
    })
}

/// The packs on offer: [`CATALOG_ENV`]'s file if set, else the built-in list.
pub fn catalog() -> &'static [CatalogEntry] {
    static CATALOG: OnceLock<Vec<CatalogEntry>> = OnceLock::new();
    CATALOG.get_or_init(|| {
        let Some(path) = std::env::var_os(CATALOG_ENV).filter(|v| !v.is_empty()) else {
            return built_in();
        };
        match std::fs::read_to_string(&path)
            .map_err(|e| e.to_string())
            .and_then(|t| places::parse_catalog(&t, true))
        {
            Ok(list) => {
                log::info!(
                    "place catalog from {}: {} pack(s)",
                    path.to_string_lossy(),
                    list.len()
                );
                list
            }
            Err(e) => {
                log::warn!(
                    "place catalog {} is not usable: {e}",
                    path.to_string_lossy()
                );
                Vec::new()
            }
        }
    })
}

/// Run once when the app starts, before any place is read: puts back a pack
/// an interrupted update had moved aside and clears half-made downloads.
pub fn startup() {
    startup_in(&fp_render::env::pack::places_dir());
}

fn startup_in(dir: &Path) {
    places::clean_up(dir);
    places::clean_up(&fp_render::env::user::dir());
}

/// Why an install failed, in the terms the person is told.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Failure {
    /// The server could not be reached or stopped answering.
    Network,
    /// What came did not match the catalog (size or SHA-256).
    Mismatch,
    /// It matched but is not a pack this version can use.
    Unusable,
    /// This device could not store it.
    Disk,
    /// A folder of the person's own already has the pack's name (one this app
    /// did not install), so nothing was downloaded over it.
    Taken,
}

impl Failure {
    fn of(e: &fp_updater::Error) -> Failure {
        use fp_updater::Error as E;
        match e {
            E::Http { .. } | E::HttpStatus { .. } => Failure::Network,
            E::SizeMismatch { .. } | E::ChecksumMismatch { .. } => Failure::Mismatch,
            E::Io { source, .. } if source.kind() == std::io::ErrorKind::UnexpectedEof => {
                Failure::Network
            }
            E::Io { action, .. } if action.contains("download interrupted") => Failure::Network,
            E::Io { .. } => Failure::Disk,
            _ => Failure::Unusable,
        }
    }
}

/// What a catalog row shows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Row {
    Available,
    /// Downloading: bytes so far and in all.
    Downloading {
        done: u64,
        total: u64,
    },
    /// Downloaded, being unpacked and checked.
    Checking,
    /// On this device. `update` when the catalog has a newer version.
    Installed {
        update: bool,
    },
    Failed(Failure),
}

/// What a source of the person's own offers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Offers {
    /// Being fetched.
    Loading,
    Ready {
        entries: Vec<CatalogEntry>,
        /// Lines of the index that were not usable and were left out.
        skipped: usize,
    },
    /// The index could not be fetched or read.
    Failed,
}

/// The most sources a person can add.
pub const MAX_SOURCES: usize = 8;

/// Why an address was not added as a source.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AddError {
    /// Not an https address on the public internet.
    Invalid,
    Duplicate,
    TooMany,
}

/// Adds the address `input` to `sources` (tidied: see
/// [`places::check_source_url`]).
pub fn add_source(sources: &mut Vec<String>, input: &str) -> Result<(), AddError> {
    let url = places::check_source_url(input).map_err(|_| AddError::Invalid)?;
    if sources.contains(&url) {
        return Err(AddError::Duplicate);
    }
    if sources.len() >= MAX_SOURCES {
        return Err(AddError::TooMany);
    }
    sources.push(url);
    Ok(())
}

/// Takes `url` out of `sources`; whether it was there.
pub fn remove_source(sources: &mut Vec<String>, url: &str) -> bool {
    let before = sources.len();
    sources.retain(|s| s != url);
    sources.len() != before
}

/// Makes `offers` hold exactly `urls`: what a removed source offered goes,
/// a new one starts [`Offers::Loading`]. The addresses to fetch.
fn reconcile(offers: &mut HashMap<String, Offers>, urls: &[String]) -> Vec<String> {
    offers.retain(|k, _| urls.contains(k));
    let mut fetch = Vec::new();
    for url in urls {
        if !offers.contains_key(url) {
            offers.insert(url.clone(), Offers::Loading);
            fetch.push(url.clone());
        }
    }
    fetch
}

/// The key a download goes by: the pack's id, and for a person's own
/// source `user:` first, so two packs of one name cannot be mistaken.
fn key(id: &str, user: bool) -> String {
    if user {
        format!("user:{id}")
    } else {
        id.to_string()
    }
}

#[derive(Default)]
struct Shared {
    /// The key being installed and its progress (done, total); total 0 while
    /// the pack is being unpacked and checked.
    busy: Option<(String, u64, u64)>,
    failed: HashMap<String, Failure>,
    /// Installed catalog packs and the version each names; `None` until
    /// first looked up.
    installed: Option<HashMap<String, Option<String>>>,
    /// The same for the packs in the person's own folder, by folder name.
    user_installed: Option<HashMap<String, Option<String>>>,
    /// What each of the person's sources offers, by address.
    sources: HashMap<String, Offers>,
    /// Something was installed or removed this run: the Place list only
    /// reads the folder at start.
    changed: bool,
}

/// The downloads in Settings > Places, shared with the worker thread.
#[derive(Default)]
pub struct PlaceDownloads {
    shared: Arc<Mutex<Shared>>,
    cancel: Arc<AtomicBool>,
}

fn scan(dir: &Path) -> HashMap<String, Option<String>> {
    catalog()
        .iter()
        .filter(|e| places::is_installed(dir, &e.id))
        .map(|e| {
            let version = fp_render::env::pack::installed_version(&dir.join(&e.id));
            (e.id.clone(), version)
        })
        .collect()
}

/// The packs this app installed in the person's folder (a manifest and the
/// install marker) and the version each names. A folder of their own, made or
/// copied by hand, is not in it: it is never offered an update or a Remove.
fn scan_user(dir: &Path) -> HashMap<String, Option<String>> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return HashMap::new();
    };
    entries
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            (places::is_installed(dir, &name) && places::is_installed_by_app(dir, &name)).then(
                || {
                    let version = fp_render::env::pack::installed_version(&e.path());
                    (name, version)
                },
            )
        })
        .collect()
}

impl PlaceDownloads {
    /// What the row for `entry` shows now; `user` for a pack one of the
    /// person's own sources offers.
    pub fn row_for(&self, entry: &CatalogEntry, user: bool) -> Row {
        let k = key(&entry.id, user);
        let mut s = lock(&self.shared);
        if let Some((id, done, total)) = &s.busy
            && *id == k
        {
            return if *total == 0 {
                Row::Checking
            } else {
                Row::Downloading {
                    done: *done,
                    total: *total,
                }
            };
        }
        if let Some(f) = s.failed.get(&k) {
            return Row::Failed(*f);
        }
        let installed = if user {
            let dir = fp_render::env::user::dir();
            let mine = s.user_installed.get_or_insert_with(|| scan_user(&dir));
            // A folder of theirs in the way is told, not offered.
            if !mine.contains_key(&entry.id) && places::is_taken_by_other(&dir, &entry.id) {
                return Row::Failed(Failure::Taken);
            }
            mine
        } else {
            s.installed
                .get_or_insert_with(|| scan(&fp_render::env::pack::places_dir()))
        };
        match installed.get(&entry.id) {
            Some(version) => Row::Installed {
                update: version.as_deref() != Some(entry.version.as_str()),
            },
            None => Row::Available,
        }
    }

    /// Whether a download is running (only one runs at a time).
    pub fn busy(&self) -> bool {
        lock(&self.shared).busy.is_some()
    }

    /// Whether something was installed or removed since the app started.
    pub fn changed(&self) -> bool {
        lock(&self.shared).changed
    }

    /// Stops the running download; nothing is kept.
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }

    /// Starts downloading `entry` into `dir` on a worker thread, unless one
    /// is running already. Whether it started.
    pub fn start(&self, ctx: &egui::Context, entry: &CatalogEntry, dir: PathBuf) -> bool {
        self.start_in(ctx, entry, dir, false)
    }

    /// [`PlaceDownloads::start`] for a pack one of the person's own sources
    /// offers: into their `environments` folder, https only.
    pub fn start_user(&self, ctx: &egui::Context, entry: &CatalogEntry) -> bool {
        self.start_in(ctx, entry, fp_render::env::user::dir(), true)
    }

    fn start_in(
        &self,
        ctx: &egui::Context,
        entry: &CatalogEntry,
        dir: PathBuf,
        user: bool,
    ) -> bool {
        let k = key(&entry.id, user);
        {
            let mut s = lock(&self.shared);
            if s.busy.is_some() {
                return false;
            }
            s.failed.remove(&k);
            s.busy = Some((k.clone(), 0, entry.bytes));
        }
        self.cancel.store(false, Ordering::Relaxed);
        let (shared, cancel) = (self.shared.clone(), self.cancel.clone());
        let (ctx, entry) = (ctx.clone(), entry.clone());
        let spawned = std::thread::Builder::new()
            .name("place-download".into())
            .spawn({
                let shared = shared.clone();
                let ctx = ctx.clone();
                let entry = entry.clone();
                let k = k.clone();
                move || {
                    let result = run(&shared, &ctx, &entry, &dir, &cancel, user);
                    let mut s = lock(&shared);
                    s.busy = None;
                    match result {
                        Ok(()) => {
                            s.changed = true;
                            if user {
                                s.user_installed = Some(scan_user(&dir));
                            } else {
                                s.installed = Some(scan(&dir));
                            }
                        }
                        Err(None) => {}
                        Err(Some(f)) => {
                            s.failed.insert(k, f);
                        }
                    }
                    drop(s);
                    ctx.request_repaint();
                }
            });
        if let Err(e) = spawned {
            log::warn!("cannot start the place download: {e}");
            let mut s = lock(&shared);
            s.busy = None;
            s.failed.insert(k, Failure::Disk);
        }
        true
    }

    /// Deletes the installed pack `id` from `dir`.
    pub fn remove(&self, id: &str, dir: &Path) {
        self.remove_in(id, dir, false);
    }

    /// Deletes a pack installed from one of the person's own sources.
    pub fn remove_user(&self, id: &str) {
        self.remove_in(id, &fp_render::env::user::dir(), true);
    }

    fn remove_in(&self, id: &str, dir: &Path, user: bool) {
        if self.busy() {
            return;
        }
        // In the person's own folder only what the app installed goes.
        let removed = if user {
            places::remove_installed_pack(dir, id)
        } else {
            places::remove_pack(dir, id)
        };
        match removed {
            Ok(()) => log::info!("place pack {id} removed"),
            Err(e) => log::warn!("cannot remove place pack {id}: {e}"),
        }
        let mut s = lock(&self.shared);
        if user {
            s.user_installed = Some(scan_user(dir));
        } else {
            s.installed = Some(scan(dir));
        }
        s.changed = true;
    }

    /// Makes what is fetched match the person's sources: a new address is
    /// fetched in the background, a removed one's offers are forgotten (what
    /// was installed from it stays). Cheap enough to call every frame.
    pub fn sync_sources(&self, ctx: &egui::Context, urls: &[String]) {
        let fetch = reconcile(&mut lock(&self.shared).sources, urls);
        for url in fetch {
            self.fetch_source(ctx, url);
        }
    }

    /// Fetches a source's index again (after a failure).
    pub fn retry_source(&self, ctx: &egui::Context, url: &str) {
        let mut s = lock(&self.shared);
        let Some(slot) = s.sources.get_mut(url) else {
            return;
        };
        *slot = Offers::Loading;
        drop(s);
        self.fetch_source(ctx, url.to_string());
    }

    /// What a source offers now.
    pub fn offers(&self, url: &str) -> Offers {
        lock(&self.shared)
            .sources
            .get(url)
            .cloned()
            .unwrap_or(Offers::Loading)
    }

    fn fetch_source(&self, ctx: &egui::Context, url: String) {
        let (shared, ctx) = (self.shared.clone(), ctx.clone());
        let spawned = std::thread::Builder::new()
            .name("place-source".into())
            .spawn({
                let (shared, ctx, url) = (shared.clone(), ctx.clone(), url.clone());
                move || {
                    let result = places::check_source_url(&url)
                        .map_err(|e| e.to_string())
                        .and_then(|u| {
                            places::fetch_index(&fp_updater::https_only_agent(), &u)
                                .map_err(|e| e.to_string())
                        });
                    let offers = match result {
                        Ok(ix) => {
                            log::info!(
                                "place source {url}: {} pack(s), {} line(s) skipped",
                                ix.entries.len(),
                                ix.skipped
                            );
                            Offers::Ready {
                                entries: ix.entries,
                                skipped: ix.skipped,
                            }
                        }
                        Err(e) => {
                            log::warn!("place source {url} is not usable: {e}");
                            Offers::Failed
                        }
                    };
                    let mut s = lock(&shared);
                    // A source removed while it was fetched stays removed.
                    if let Some(slot) = s.sources.get_mut(&url) {
                        *slot = offers;
                    }
                    drop(s);
                    ctx.request_repaint();
                }
            });
        if let Err(e) = spawned {
            log::warn!("cannot start fetching a place source: {e}");
            if let Some(slot) = lock(&shared).sources.get_mut(&url) {
                *slot = Offers::Failed;
            }
        }
    }
}

/// The worker: `Err(None)` is a cancel, `Err(Some(why))` a failure.
fn run(
    shared: &Mutex<Shared>,
    ctx: &egui::Context,
    entry: &CatalogEntry,
    dir: &Path,
    cancel: &AtomicBool,
    user: bool,
) -> Result<(), Option<Failure>> {
    // A folder of theirs with this name is left alone, before any download.
    if user && places::is_taken_by_other(dir, &entry.id) {
        log::warn!(
            "place pack {} not installed: {} is a folder of yours",
            entry.id,
            entry.id
        );
        return Err(Some(Failure::Taken));
    }
    places::clean_up(dir);
    // https only, unless the catalog file names this machine and nothing
    // else; a person's own source is always https only.
    let agent = if user || places::needs_https_only(catalog()) {
        fp_updater::https_only_agent()
    } else {
        fp_updater::default_agent()
    };
    let mut percent = u64::MAX;
    let progress = |done: u64, total: u64| {
        let now = (done * 100).checked_div(total).unwrap_or(0);
        let mut s = lock(shared);
        if let Some(b) = s.busy.as_mut() {
            // The bytes are in; the pack is unpacked and checked next.
            *b = (b.0.clone(), done, if done >= total { 0 } else { total });
        }
        drop(s);
        if now != percent {
            percent = now;
            ctx.request_repaint();
        }
    };
    let id = entry.id.clone();
    let validate = move |pack: &Path| {
        fp_render::env::pack::validate_download(pack, &id)
            .map(|version| log::info!("place pack {id} {version} passed its checks"))
            .map_err(|e| e.to_string())
    };
    match places::install_pack_in(&agent, entry, dir, user, validate, progress, cancel) {
        Ok(path) => {
            log::info!("place pack {} installed at {}", entry.id, path.display());
            Ok(())
        }
        Err(fp_updater::Error::Cancelled) => {
            log::info!("place pack {} download cancelled", entry.id);
            Err(None)
        }
        Err(e) => {
            log::warn!("place pack {} was not installed: {e}", entry.id);
            Err(Some(Failure::of(&e)))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(id: &str) -> CatalogEntry {
        CatalogEntry {
            id: id.into(),
            version: "1".into(),
            url: "https://github.com/x/y.zip".into(),
            sha256: "0".repeat(64),
            bytes: 100,
            title: "Campsite".into(),
        }
    }

    #[test]
    fn a_start_while_another_download_runs_is_refused_and_says_so() {
        let d = PlaceDownloads::default();
        lock(&d.shared).busy = Some(("other".into(), 1, 10));
        let tmp = std::env::temp_dir().join("laterna-start-busy-test");
        assert!(!d.start(&egui::Context::default(), &entry("campsite"), tmp.clone()));
        // Nothing about the refused pack changed and the first still runs.
        assert_eq!(lock(&d.shared).busy.as_ref().unwrap().0, "other");
        assert!(!tmp.exists());
    }

    #[test]
    fn startup_restores_an_interrupted_update_and_clears_half_downloads() {
        let tmp = std::env::temp_dir().join(format!("laterna-startup-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let places = tmp.as_path();
        std::fs::create_dir_all(places.join(".campsite.old")).unwrap();
        std::fs::write(places.join(".campsite.old/manifest.json"), b"{}").unwrap();
        std::fs::create_dir_all(places.join(".other.part")).unwrap();
        startup_in(places);
        assert!(places.join("campsite/manifest.json").is_file());
        assert!(!places.join(".campsite.old").exists());
        assert!(!places.join(".other.part").exists());
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn the_built_in_catalog_is_well_formed() {
        // Empty today; when a pack is added its line must parse.
        let list = places::parse_catalog(BUILT_IN, false).expect("places-optional.lock parses");
        assert!(list.iter().all(|e| e.bytes <= places::MAX_PACK_BYTES));
    }

    #[test]
    fn failures_are_told_by_cause() {
        use fp_updater::Error as E;
        let io = |kind| E::Io {
            action: "cannot write",
            path: PathBuf::new(),
            source: std::io::Error::from(kind),
        };
        assert_eq!(
            Failure::of(&E::HttpStatus {
                url: "u".into(),
                status: 404
            }),
            Failure::Network
        );
        assert_eq!(
            Failure::of(&E::SizeMismatch {
                url: "u".into(),
                expected: 1,
                actual: 2
            }),
            Failure::Mismatch
        );
        assert_eq!(Failure::of(&E::InvalidPack("x".into())), Failure::Unusable);
        assert_eq!(
            Failure::of(&io(std::io::ErrorKind::PermissionDenied)),
            Failure::Disk
        );
        assert_eq!(
            Failure::of(&E::Io {
                action: "download interrupted while reading",
                path: PathBuf::new(),
                source: std::io::Error::from(std::io::ErrorKind::ConnectionReset),
            }),
            Failure::Network
        );
    }

    #[test]
    fn a_failure_and_a_cancel_leave_the_row_ready_for_another_try() {
        let d = PlaceDownloads::default();
        lock(&d.shared).installed = Some(HashMap::new());
        let entry = CatalogEntry {
            id: "campsite".into(),
            version: "1".into(),
            url: "https://github.com/x/y.zip".into(),
            sha256: "0".repeat(64),
            bytes: 100,
            title: "Campsite".into(),
        };
        assert_eq!(d.row_for(&entry, false), Row::Available);
        lock(&d.shared)
            .failed
            .insert("campsite".into(), Failure::Network);
        assert_eq!(d.row_for(&entry, false), Row::Failed(Failure::Network));
        lock(&d.shared).failed.remove("campsite");
        assert_eq!(d.row_for(&entry, false), Row::Available);
        lock(&d.shared).busy = Some(("campsite".into(), 40, 100));
        assert_eq!(
            d.row_for(&entry, false),
            Row::Downloading {
                done: 40,
                total: 100
            }
        );
        lock(&d.shared).busy = Some(("campsite".into(), 100, 0));
        assert_eq!(d.row_for(&entry, false), Row::Checking);
        lock(&d.shared).busy = None;
        lock(&d.shared)
            .installed
            .as_mut()
            .unwrap()
            .insert("campsite".into(), Some("0".into()));
        assert_eq!(d.row_for(&entry, false), Row::Installed { update: true });
        lock(&d.shared)
            .installed
            .as_mut()
            .unwrap()
            .insert("campsite".into(), Some("1".into()));
        assert_eq!(d.row_for(&entry, false), Row::Installed { update: false });
    }

    #[test]
    fn sources_are_added_tidied_and_removed() {
        let mut list = Vec::new();
        assert_eq!(
            add_source(&mut list, "  https://example.com/a.lock "),
            Ok(())
        );
        assert_eq!(list, ["https://example.com/a.lock"]);
        assert_eq!(
            add_source(&mut list, "https://example.com/a.lock"),
            Err(AddError::Duplicate)
        );
        for bad in [
            "",
            "http://example.com/a",
            "https://192.168.1.2/a",
            "nonsense",
        ] {
            assert_eq!(
                add_source(&mut list, bad),
                Err(AddError::Invalid),
                "{bad:?}"
            );
        }
        for n in 1..MAX_SOURCES {
            add_source(&mut list, &format!("https://example.com/{n}.lock")).unwrap();
        }
        assert_eq!(
            add_source(&mut list, "https://example.com/more.lock"),
            Err(AddError::TooMany)
        );
        assert!(remove_source(&mut list, "https://example.com/a.lock"));
        assert!(!remove_source(&mut list, "https://example.com/a.lock"));
        assert_eq!(list.len(), MAX_SOURCES - 1);
    }

    #[test]
    fn removing_a_source_removes_its_offers_and_adding_one_asks_for_them() {
        let mut offers: HashMap<String, Offers> = HashMap::new();
        let (a, b) = (
            "https://a.example/x".to_string(),
            "https://b.example/x".to_string(),
        );
        let both = [a.clone(), b.clone()];
        assert_eq!(reconcile(&mut offers, &both), both);
        assert_eq!(offers.get(&a), Some(&Offers::Loading));
        // Offers arrive for a; nothing is asked again for a source we have.
        offers.insert(
            a.clone(),
            Offers::Ready {
                entries: vec![entry("campsite")],
                skipped: 0,
            },
        );
        assert!(reconcile(&mut offers, &both).is_empty());
        // Remove a: its offers go; b stays.
        assert!(reconcile(&mut offers, std::slice::from_ref(&b)).is_empty());
        assert!(!offers.contains_key(&a));
        assert!(offers.contains_key(&b));
        // Put a back: it is fetched again, and starts empty.
        assert_eq!(reconcile(&mut offers, &both), [a.as_str()]);
        assert_eq!(offers.get(&a), Some(&Offers::Loading));
        // No sources: nothing at all.
        assert!(reconcile(&mut offers, &[]).is_empty());
        assert!(offers.is_empty());
    }

    #[test]
    fn a_source_removed_while_it_loads_stays_removed_and_installed_packs_stay() {
        let d = PlaceDownloads::default();
        let url = "https://a.example/x".to_string();
        let fetch = reconcile(&mut lock(&d.shared).sources, std::slice::from_ref(&url));
        assert_eq!(fetch.len(), 1);
        assert_eq!(d.offers(&url), Offers::Loading);
        reconcile(&mut lock(&d.shared).sources, &[]);
        // What a pack installed from it left in the folder is not a source's.
        let tmp = std::env::temp_dir().join(format!("laterna-user-src-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(tmp.join("campsite")).unwrap();
        std::fs::write(tmp.join("campsite/manifest.json"), b"{}").unwrap();
        std::fs::write(tmp.join("campsite").join(places::INSTALLED_MARKER), b"1").unwrap();
        assert!(scan_user(&tmp).contains_key("campsite"));
        std::fs::create_dir_all(tmp.join(".half.part")).unwrap();
        std::fs::write(tmp.join(".half.part/manifest.json"), b"{}").unwrap();
        assert_eq!(scan_user(&tmp).len(), 1);
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn a_pack_the_person_made_by_hand_is_not_installed_updated_or_removed() {
        let d = PlaceDownloads::default();
        let tmp = std::env::temp_dir().join(format!("laterna-user-own-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        // Theirs, under the id an offer has, and one the app installed.
        std::fs::create_dir_all(tmp.join("campsite")).unwrap();
        std::fs::write(tmp.join("campsite/manifest.json"), b"{}").unwrap();
        std::fs::create_dir_all(tmp.join("lodge")).unwrap();
        std::fs::write(tmp.join("lodge/manifest.json"), b"{}").unwrap();
        std::fs::write(tmp.join("lodge").join(places::INSTALLED_MARKER), b"1").unwrap();
        let found = scan_user(&tmp);
        assert!(
            !found.contains_key("campsite"),
            "their folder shows as installed"
        );
        assert!(found.contains_key("lodge"));
        // Remove leaves theirs and takes the app's.
        d.remove_in("campsite", &tmp, true);
        assert!(tmp.join("campsite/manifest.json").is_file());
        d.remove_in("lodge", &tmp, true);
        assert!(!tmp.join("lodge").exists());
        // An install over theirs is refused before any download.
        let ctx = egui::Context::default();
        let r = run(
            &d.shared,
            &ctx,
            &entry("campsite"),
            &tmp,
            &AtomicBool::new(false),
            true,
        );
        assert_eq!(r, Err(Some(Failure::Taken)));
        assert!(tmp.join("campsite/manifest.json").is_file());
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn a_users_download_is_told_apart_from_the_catalogs_of_the_same_name() {
        let d = PlaceDownloads::default();
        lock(&d.shared).installed = Some(HashMap::new());
        lock(&d.shared).user_installed = Some(HashMap::new());
        let e = entry("campsite");
        lock(&d.shared).busy = Some(("user:campsite".into(), 5, 10));
        assert_eq!(d.row_for(&e, false), Row::Available);
        assert_eq!(d.row_for(&e, true), Row::Downloading { done: 5, total: 10 });
        lock(&d.shared).busy = None;
        lock(&d.shared)
            .failed
            .insert("user:campsite".into(), Failure::Mismatch);
        assert_eq!(d.row_for(&e, false), Row::Available);
        assert_eq!(d.row_for(&e, true), Row::Failed(Failure::Mismatch));
        lock(&d.shared).failed.clear();
        lock(&d.shared)
            .user_installed
            .as_mut()
            .unwrap()
            .insert("campsite".into(), Some("1".into()));
        assert_eq!(d.row_for(&e, true), Row::Installed { update: false });
        assert_eq!(d.row_for(&e, false), Row::Available);
    }
}
