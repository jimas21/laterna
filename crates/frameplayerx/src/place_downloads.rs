//! Optional places the person can download from Settings > Places.
//!
//! The list of packs is built into the app (`packaging/places-optional.lock`,
//! so each release vouches for every pack's SHA-256 and size) and is empty
//! until a pack is published. This module holds the state of the one
//! download that may run at a time, on a worker thread, and the installed
//! packs; `fp_updater::places` does the work and `fp_render::env::pack`
//! decides whether a downloaded pack is acceptable.
//!
//! `LATERNA_PLACES_CATALOG=<file>` replaces the built-in list with a file of
//! the same format, and allows plain http to this machine. It is for tests
//! and the desktop preview; the hashes are still checked.

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

/// Whether the catalog comes from [`CATALOG_ENV`].
pub fn catalog_overridden() -> bool {
    std::env::var_os(CATALOG_ENV).is_some_and(|v| !v.is_empty())
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

#[derive(Default)]
struct Shared {
    /// The id being installed and its progress (done, total); total 0 while
    /// the pack is being unpacked and checked.
    busy: Option<(String, u64, u64)>,
    failed: HashMap<String, Failure>,
    /// Installed catalog packs and the version each names; `None` until
    /// first looked up.
    installed: Option<HashMap<String, Option<String>>>,
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

impl PlaceDownloads {
    /// What the row for `entry` shows now.
    pub fn row(&self, entry: &CatalogEntry) -> Row {
        let mut s = lock(&self.shared);
        if let Some((id, done, total)) = &s.busy
            && *id == entry.id
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
        if let Some(f) = s.failed.get(&entry.id) {
            return Row::Failed(*f);
        }
        let installed = s
            .installed
            .get_or_insert_with(|| scan(&fp_render::env::pack::places_dir()));
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

    /// Forgets a failure so the row offers the download again.
    pub fn dismiss(&self, id: &str) {
        lock(&self.shared).failed.remove(id);
    }

    /// Stops the running download; nothing is kept.
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }

    /// Starts downloading `entry` into `dir` on a worker thread, unless one
    /// is running already.
    pub fn start(&self, ctx: &egui::Context, entry: &CatalogEntry, dir: PathBuf) {
        {
            let mut s = lock(&self.shared);
            if s.busy.is_some() {
                return;
            }
            s.failed.remove(&entry.id);
            s.busy = Some((entry.id.clone(), 0, entry.bytes));
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
                move || {
                    let result = run(&shared, &ctx, &entry, &dir, &cancel);
                    let mut s = lock(&shared);
                    s.busy = None;
                    match result {
                        Ok(()) => {
                            s.changed = true;
                            s.installed = Some(scan(&dir));
                        }
                        Err(None) => {}
                        Err(Some(f)) => {
                            s.failed.insert(entry.id.clone(), f);
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
            s.failed.insert(entry.id, Failure::Disk);
        }
    }

    /// Deletes the installed pack `id` from `dir`.
    pub fn remove(&self, id: &str, dir: &Path) {
        if self.busy() {
            return;
        }
        match places::remove_pack(dir, id) {
            Ok(()) => log::info!("place pack {id} removed"),
            Err(e) => log::warn!("cannot remove place pack {id}: {e}"),
        }
        let mut s = lock(&self.shared);
        s.installed = Some(scan(dir));
        s.changed = true;
    }
}

/// The worker: `Err(None)` is a cancel, `Err(Some(why))` a failure.
fn run(
    shared: &Mutex<Shared>,
    ctx: &egui::Context,
    entry: &CatalogEntry,
    dir: &Path,
    cancel: &AtomicBool,
) -> Result<(), Option<Failure>> {
    places::clean_up(dir);
    let agent = if catalog_overridden() {
        fp_updater::default_agent()
    } else {
        fp_updater::https_only_agent()
    };
    let mut percent = u64::MAX;
    let progress = |done: u64, total: u64| {
        let now = if total == 0 { 0 } else { done * 100 / total };
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
    match places::install_pack(&agent, entry, dir, validate, progress, cancel) {
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
        assert_eq!(d.row(&entry), Row::Available);
        lock(&d.shared)
            .failed
            .insert("campsite".into(), Failure::Network);
        assert_eq!(d.row(&entry), Row::Failed(Failure::Network));
        d.dismiss("campsite");
        assert_eq!(d.row(&entry), Row::Available);
        lock(&d.shared).busy = Some(("campsite".into(), 40, 100));
        assert_eq!(
            d.row(&entry),
            Row::Downloading {
                done: 40,
                total: 100
            }
        );
        lock(&d.shared).busy = Some(("campsite".into(), 100, 0));
        assert_eq!(d.row(&entry), Row::Checking);
        lock(&d.shared).busy = None;
        lock(&d.shared)
            .installed
            .as_mut()
            .unwrap()
            .insert("campsite".into(), Some("0".into()));
        assert_eq!(d.row(&entry), Row::Installed { update: true });
        lock(&d.shared)
            .installed
            .as_mut()
            .unwrap()
            .insert("campsite".into(), Some("1".into()));
        assert_eq!(d.row(&entry), Row::Installed { update: false });
    }
}
