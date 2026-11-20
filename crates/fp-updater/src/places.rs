//! Optional place packs: the catalog compiled into the app, and downloading,
//! checking, installing and removing one pack.
//!
//! The catalog is a text file in the repository (`packaging/places-optional.lock`)
//! built into the app, so the signed app release vouches for every pack's
//! SHA-256 and size; there is no index to fetch. An entry is one line:
//!
//! ```text
//! <id> <version> <https url> <sha256> <bytes> <title...>
//! ```
//!
//! [`install_pack`] downloads the zip into `<places dir>/.<id>.part/`, checks
//! its size and SHA-256 against the entry, unpacks it by the same rules as
//! `tools/fetch-places.sh` (everything inside `<id>/`, no absolute or `..`
//! names, no links, regular files only), runs the caller's pack checks on the
//! result and only then renames it to `<places dir>/<id>/`. Any failure
//! removes everything it made. [`remove_pack`] deletes an installed pack.

use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

use crate::client::{Target, download_file};
use crate::error::{Error, IoContext, Result};
use crate::install::{extract_zip_capped, safe_entry_components};
use crate::url::{check_download_url, is_loopback_host, parse_url};

/// The most a pack's zip may be, and so the most the app will download.
pub const MAX_PACK_BYTES: u64 = 40 << 20;
/// The most a pack may hold once unpacked. A format 3 pack has a set for each
/// of its three seats, each with up to 32 MiB of textures (the renderer's
/// `MAX_TEXTURE_BYTES`) and a mesh of up to about 5.2 MB, plus its manifest:
/// 3 x 32 MiB and 16 MiB for the rest.
pub const MAX_UNPACKED_BYTES: u64 = 112 << 20;
/// The most files a pack's zip may hold.
const MAX_ENTRIES: usize = 64;

/// One optional pack the app knows about.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CatalogEntry {
    /// The pack's id: lowercase letters, digits and hyphens. Also the name
    /// of its folder, and the id in its `manifest.json`.
    pub id: String,
    /// Shown, and compared with an installed pack's own version to offer an
    /// update.
    pub version: String,
    pub url: String,
    /// Lower-case hex SHA-256 of the zip.
    pub sha256: String,
    /// Size of the zip in bytes: what the download must come to exactly.
    pub bytes: u64,
    /// The name shown in Settings.
    pub title: String,
}

impl CatalogEntry {
    /// The host the zip comes from, for telling the person before the first
    /// download (`github.com`).
    pub fn host(&self) -> String {
        parse_url(&self.url)
            .map(|p| p.host.to_string())
            .unwrap_or_default()
    }
}

fn id_ok(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        && !id.starts_with('-')
        && !id.ends_with('-')
}

/// Whether `url` may be fetched: https, or (`allow_local_http`, tests and
/// the preview only) plain http to this machine.
fn url_ok(url: &str, allow_local_http: bool) -> std::result::Result<(), String> {
    check_download_url(url).map_err(|e| e.to_string())?;
    let https = parse_url(url).is_ok_and(|p| p.scheme.eq_ignore_ascii_case("https"));
    if !https && !allow_local_http {
        return Err("only https is allowed".into());
    }
    Ok(())
}

fn check_entry(e: &CatalogEntry, allow_local_http: bool) -> std::result::Result<(), String> {
    if !id_ok(&e.id) {
        return Err(format!("{:?} is not a pack id", e.id));
    }
    if e.version.is_empty()
        || e.version.len() > 32
        || !e
            .version
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-+".contains(&b))
    {
        return Err(format!("{:?} is not a version", e.version));
    }
    url_ok(&e.url, allow_local_http)?;
    if e.sha256.len() != 64
        || !e
            .sha256
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err("sha256 must be 64 lower-case hex digits".into());
    }
    if e.bytes == 0 || e.bytes > MAX_PACK_BYTES {
        return Err(format!("{} bytes (1 to {MAX_PACK_BYTES} allowed)", e.bytes));
    }
    if e.title.is_empty() || e.title.chars().count() > 60 || e.title.chars().any(char::is_control) {
        return Err("the title must be 1 to 60 characters".into());
    }
    Ok(())
}

/// Parses a catalog (blank lines and `#` comments are skipped). Any bad line
/// is an error naming it: the catalog is compiled in and tested, so a bad one
/// is a mistake to find, not input to put up with. `allow_local_http` lets
/// an entry use plain http to this machine; it is for tests and the preview.
pub fn parse_catalog(
    text: &str,
    allow_local_http: bool,
) -> std::result::Result<Vec<CatalogEntry>, String> {
    let mut out: Vec<CatalogEntry> = Vec::new();
    for (n, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let at = |m: String| format!("line {}: {m}", n + 1);
        let mut f = line.splitn(6, char::is_whitespace);
        let (Some(id), Some(version), Some(url), Some(sha256), Some(bytes), Some(title)) =
            (f.next(), f.next(), f.next(), f.next(), f.next(), f.next())
        else {
            return Err(at("want: id version url sha256 bytes title".into()));
        };
        let bytes = bytes
            .parse::<u64>()
            .map_err(|_| at(format!("{bytes:?} is not a byte count")))?;
        let entry = CatalogEntry {
            id: id.into(),
            version: version.into(),
            url: url.into(),
            sha256: sha256.into(),
            bytes,
            title: title.trim().into(),
        };
        check_entry(&entry, allow_local_http).map_err(at)?;
        if out.iter().any(|e| e.id == entry.id) {
            return Err(at(format!("{} twice", entry.id)));
        }
        out.push(entry);
    }
    Ok(out)
}

/// Whether downloads for this catalog must use the https-only agent: always,
/// unless the catalog is not empty and every entry's host is this machine
/// (a test or preview catalog). One remote entry keeps the whole catalog on
/// https, so a catalog file cannot switch TLS off for a real host.
pub fn needs_https_only(catalog: &[CatalogEntry]) -> bool {
    catalog.is_empty()
        || !catalog
            .iter()
            .all(|e| parse_url(&e.url).is_ok_and(|p| is_loopback_host(p.host)))
}

/// Where a pack is unpacked and checked before it is moved into place.
fn staging_dir(places_dir: &Path, id: &str) -> PathBuf {
    places_dir.join(format!(".{id}.part"))
}

/// Where an installed pack being replaced waits until the new one is in.
fn old_dir(places_dir: &Path, id: &str) -> PathBuf {
    places_dir.join(format!(".{id}.old"))
}

fn remove_tree(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(Error::io("cannot look at", path, e)),
        Ok(m) if m.file_type().is_dir() => fs::remove_dir_all(path).ctx("cannot remove", path),
        Ok(_) => fs::remove_file(path).ctx("cannot remove", path),
    }
}

/// Clears what an install that was cut off (the app closed, the machine
/// died) left in `places_dir`: half-made `.<id>.part` folders go, and a pack
/// that was being replaced (`.<id>.old`) goes back if its new one never
/// arrived. Call it when no install is running.
pub fn clean_up(places_dir: &Path) {
    let Ok(entries) = fs::read_dir(places_dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let Some(rest) = name.strip_prefix('.') else {
            continue;
        };
        if rest.ends_with(".part") {
            let _ = remove_tree(&entry.path());
        } else if let Some(id) = rest.strip_suffix(".old") {
            let dest = places_dir.join(id);
            if fs::symlink_metadata(&dest).is_ok() || !id_ok(id) {
                let _ = remove_tree(&entry.path());
            } else {
                let _ = fs::rename(entry.path(), dest);
            }
        }
    }
}

/// The file the app writes into every pack folder it installs. A folder in
/// the person's own `environments` folder without it is theirs (made or
/// copied by hand): the app never replaces it, removes it or offers an update
/// for it.
pub const INSTALLED_MARKER: &str = ".laterna-installed";

/// Whether the pack `id` is installed in `places_dir` (a folder with a
/// `manifest.json`; whether it still loads is the app's business).
pub fn is_installed(places_dir: &Path, id: &str) -> bool {
    id_ok(id) && places_dir.join(id).join("manifest.json").is_file()
}

/// Whether the folder `id` in `places_dir` is a pack this app installed (it
/// holds the marker, a plain file, not a link).
pub fn is_installed_by_app(places_dir: &Path, id: &str) -> bool {
    id_ok(id)
        && fs::symlink_metadata(places_dir.join(id).join(INSTALLED_MARKER))
            .is_ok_and(|m| m.file_type().is_file())
}

/// Whether something is in the way of installing the pack `id`: a folder (or
/// file, or link) of that name that this app did not install.
pub fn is_taken_by_other(places_dir: &Path, id: &str) -> bool {
    id_ok(id)
        && fs::symlink_metadata(places_dir.join(id)).is_ok()
        && !is_installed_by_app(places_dir, id)
}

/// Deletes the installed pack `id`. Nothing installed is not an error.
pub fn remove_pack(places_dir: &Path, id: &str) -> Result<()> {
    if !id_ok(id) {
        return Err(Error::InvalidPack(format!("{id:?} is not a pack id")));
    }
    remove_tree(&places_dir.join(id))
}

/// [`remove_pack`] for a folder the person also keeps their own packs in:
/// only a pack this app installed is deleted; anything else of that name is
/// left alone and is an error.
pub fn remove_installed_pack(places_dir: &Path, id: &str) -> Result<()> {
    if is_taken_by_other(places_dir, id) {
        return Err(Error::InvalidPack(format!(
            "{id} is a folder of yours, not one this app installed"
        )));
    }
    remove_pack(places_dir, id)
}

/// Checks a downloaded zip before anything is unpacked: not too many files,
/// nothing outside `<id>/`, no unsafe name, a `manifest.json`, and a declared
/// size within [`MAX_UNPACKED_BYTES`]. Links and special files are refused by
/// the extraction itself.
fn inspect(zip_path: &Path, id: &str) -> Result<()> {
    let file = File::open(zip_path).ctx("cannot open", zip_path)?;
    let mut archive = zip::ZipArchive::new(file)?;
    if archive.is_empty() || archive.len() > MAX_ENTRIES {
        return Err(Error::ArchiveTooLarge(format!(
            "{} files (1 to {MAX_ENTRIES} allowed)",
            archive.len()
        )));
    }
    let prefix = format!("{id}/");
    let manifest = format!("{id}/manifest.json");
    let mut has_manifest = false;
    let mut total = 0u64;
    for i in 0..archive.len() {
        let entry = archive.by_index(i)?;
        let name = entry.name()?.into_owned();
        safe_entry_components(&name)?;
        if !name.starts_with(&prefix) {
            return Err(Error::InvalidPack(format!("{name:?} is outside {prefix}")));
        }
        has_manifest |= name == manifest;
        total = total.saturating_add(entry.size());
    }
    if !has_manifest {
        return Err(Error::InvalidPack(format!("no {manifest}")));
    }
    if total > MAX_UNPACKED_BYTES {
        return Err(Error::ArchiveTooLarge(format!(
            "{total} bytes unpacked (at most {MAX_UNPACKED_BYTES})"
        )));
    }
    Ok(())
}

/// Every file under `dir` is a regular file or folder, never a link.
fn only_plain_files(dir: &Path) -> Result<()> {
    for entry in fs::read_dir(dir).ctx("cannot read", dir)? {
        let entry = entry.ctx("cannot read", dir)?;
        let kind = entry.file_type().ctx("cannot look at", &entry.path())?;
        if kind.is_dir() {
            only_plain_files(&entry.path())?;
        } else if !kind.is_file() {
            return Err(Error::UnsupportedArchiveEntry(
                entry.file_name().to_string_lossy().into_owned(),
            ));
        }
    }
    Ok(())
}

/// Downloads, checks and installs the pack `entry` into `places_dir`, and
/// returns the new folder.
///
/// `validate` gets the unpacked folder and says whether the app will accept it
/// (the app's own pack loader, with its limits); an `Err(reason)` ends the
/// install. `progress(done, total)` follows the download. Setting `cancel`
/// stops it with [`Error::Cancelled`]. Whatever happens, nothing is left but
/// what was there before: an older install of the same pack stays until the
/// new one has passed every check, then is replaced in one rename.
pub fn install_pack(
    agent: &ureq::Agent,
    entry: &CatalogEntry,
    places_dir: &Path,
    validate: impl FnOnce(&Path) -> std::result::Result<(), String>,
    progress: impl FnMut(u64, u64),
    cancel: &AtomicBool,
) -> Result<PathBuf> {
    install_pack_in(agent, entry, places_dir, false, validate, progress, cancel)
}

/// [`install_pack`], for a folder that may hold packs of the person's own.
/// `keep_others` refuses (before anything is downloaded, and again before
/// anything is replaced) to touch a folder of that name that this app did not
/// install. Every pack this installs gets [`INSTALLED_MARKER`].
pub fn install_pack_in(
    agent: &ureq::Agent,
    entry: &CatalogEntry,
    places_dir: &Path,
    keep_others: bool,
    validate: impl FnOnce(&Path) -> std::result::Result<(), String>,
    progress: impl FnMut(u64, u64),
    cancel: &AtomicBool,
) -> Result<PathBuf> {
    // The catalog text is checked when it is parsed; this stops a hand-made
    // entry from naming a folder outside places_dir, or an endless download.
    // A plain http address is the caller's agent to refuse (the app's is
    // https-only); loopback http is allowed by the URL rule for tests.
    check_entry(entry, true).map_err(Error::InvalidPack)?;
    if keep_others && is_taken_by_other(places_dir, &entry.id) {
        return Err(taken_error(&entry.id));
    }
    fs::create_dir_all(places_dir).ctx("cannot create", places_dir)?;
    let staging = staging_dir(places_dir, &entry.id);
    remove_tree(&staging)?;
    fs::create_dir(&staging).ctx("cannot create", &staging)?;
    let result = fetch_and_place(
        agent,
        entry,
        places_dir,
        &staging,
        keep_others,
        validate,
        progress,
        cancel,
    );
    // The zip, the unpacked copy: gone, whether it worked or not.
    let _ = remove_tree(&staging);
    result
}

fn taken_error(id: &str) -> Error {
    Error::InvalidPack(format!(
        "{id} is a folder of yours, not one this app installed, so it was left alone"
    ))
}

#[allow(clippy::too_many_arguments)]
fn fetch_and_place(
    agent: &ureq::Agent,
    entry: &CatalogEntry,
    places_dir: &Path,
    staging: &Path,
    keep_others: bool,
    validate: impl FnOnce(&Path) -> std::result::Result<(), String>,
    progress: impl FnMut(u64, u64),
    cancel: &AtomicBool,
) -> Result<PathBuf> {
    let fallback = format!("{}.zip", entry.id);
    let target = Target {
        url: &entry.url,
        sha256: &entry.sha256,
        size: entry.bytes,
        fallback_name: &fallback,
    };
    // Size and SHA-256 are checked here, against the entry the app was
    // built with: a longer or different body is refused, never kept.
    let zip = download_file(agent, &target, &staging.join("dl"), progress, cancel)?;
    inspect(&zip, &entry.id)?;
    let unpacked = staging.join("pack");
    extract_zip_capped(&zip, &unpacked, MAX_UNPACKED_BYTES)?;
    only_plain_files(&unpacked)?;
    validate(&unpacked).map_err(Error::InvalidPack)?;
    // What marks it as this app's, written before it is moved in so a pack
    // is never in the folder without it.
    let marker = unpacked.join(INSTALLED_MARKER);
    fs::write(&marker, entry.version.as_bytes()).ctx("cannot write", &marker)?;

    let dest = places_dir.join(&entry.id);
    // Checked again right before the replace: the download took a while, and
    // the person may have put a folder there since.
    if keep_others && is_taken_by_other(places_dir, &entry.id) {
        return Err(taken_error(&entry.id));
    }
    let old = old_dir(places_dir, &entry.id);
    remove_tree(&old)?;
    let replacing = fs::symlink_metadata(&dest).is_ok();
    if replacing {
        fs::rename(&dest, &old).ctx("cannot move aside", &dest)?;
    }
    if let Err(e) = fs::rename(&unpacked, &dest) {
        if replacing {
            let _ = fs::rename(&old, &dest);
        }
        return Err(Error::io("cannot move into place", &dest, e));
    }
    let _ = remove_tree(&old);
    Ok(dest)
}

/// The most a person's own pack index may be, and so the most the app reads.
pub const MAX_INDEX_BYTES: u64 = 64 << 10;
/// The most packs one index may offer; the rest are ignored.
pub const MAX_INDEX_ENTRIES: usize = 50;

/// A pack index a person added, as read: what it offers and how many of its
/// lines were left out because they were not usable.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Index {
    pub entries: Vec<CatalogEntry>,
    pub skipped: usize,
}

/// Checks and tidies the address of a person's pack index: https, no
/// credentials, a host on the public internet, one line.
pub fn check_source_url(url: &str) -> std::result::Result<String, String> {
    let url = url.trim();
    if url.is_empty() || url.len() > 300 || url.chars().any(|c| c.is_whitespace() || c.is_control())
    {
        return Err("not an address".into());
    }
    crate::url::check_public_https_url(url)?;
    Ok(url.to_string())
}

/// Reads the text of a pack index in the catalog format. Unlike the built-in
/// catalog (a mistake there is a bug to find), a person's index comes from
/// somebody else: a line that is not usable is left out and counted, never
/// the whole index. Every pack's address must be https on the public
/// internet, so an index cannot point the app at this machine or the
/// person's network.
pub fn parse_index(text: &str) -> Index {
    let mut out = Index::default();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let entry = parse_catalog(line, false)
            .ok()
            .and_then(|mut v| v.pop())
            .filter(|e| crate::url::check_public_https_url(&e.url).is_ok())
            .filter(|e| !out.entries.iter().any(|o| o.id == e.id));
        match entry {
            Some(e) if out.entries.len() < MAX_INDEX_ENTRIES => out.entries.push(e),
            _ => out.skipped += 1,
        }
    }
    out
}

/// Fetches and reads the pack index at `url` (already through
/// [`check_source_url`]), through `agent` (the app's is https-only). At most
/// [`MAX_INDEX_BYTES`] are read.
pub fn fetch_index(agent: &ureq::Agent, url: &str) -> Result<Index> {
    let http = |e: ureq::Error| Error::Http {
        url: url.to_string(),
        source: Box::new(e),
    };
    let resp = agent.get(url).call().map_err(http)?;
    let status = resp.status().as_u16();
    if status != 200 {
        return Err(Error::HttpStatus {
            url: url.to_string(),
            status,
        });
    }
    let bytes = resp
        .into_body()
        .into_with_config()
        .limit(MAX_INDEX_BYTES)
        .read_to_vec()
        .map_err(http)?;
    let text = String::from_utf8(bytes)
        .map_err(|_| Error::InvalidPack("the index is not text".to_string()))?;
    Ok(parse_index(&text))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::install::tests::{Item, make_zip};
    use sha2::{Digest, Sha256};
    use std::collections::HashMap;

    use std::sync::{Arc, Mutex};
    use std::thread;

    /// What the test server does for a path.
    #[derive(Clone)]
    enum Route {
        Bytes(Vec<u8>),
        Redirect(String),
    }

    struct Server {
        base: String,
        routes: Arc<Mutex<HashMap<String, Route>>>,
        hits: Arc<Mutex<Vec<String>>>,
        server: Arc<tiny_http::Server>,
        handle: Option<thread::JoinHandle<()>>,
    }

    impl Server {
        fn start() -> Server {
            let server = Arc::new(tiny_http::Server::http("127.0.0.1:0").unwrap());
            let port = server.server_addr().to_ip().unwrap().port();
            let routes: Arc<Mutex<HashMap<String, Route>>> = Arc::default();
            let hits: Arc<Mutex<Vec<String>>> = Arc::default();
            let (s, r, h) = (server.clone(), routes.clone(), hits.clone());
            let handle = thread::spawn(move || {
                for req in s.incoming_requests() {
                    h.lock().unwrap().push(req.url().to_string());
                    let route = r.lock().unwrap().get(req.url()).cloned();
                    use tiny_http::{Header, Response, StatusCode};
                    match route {
                        None => {
                            let _ = req.respond(Response::empty(StatusCode(404)));
                        }
                        Some(Route::Bytes(data)) => {
                            let _ = req.respond(Response::from_data(data));
                        }
                        Some(Route::Redirect(to)) => {
                            let loc = Header::from_bytes("Location", to).unwrap();
                            let _ = req.respond(Response::empty(StatusCode(302)).with_header(loc));
                        }
                    }
                }
            });
            Server {
                base: format!("http://127.0.0.1:{port}"),
                routes,
                hits,
                server,
                handle: Some(handle),
            }
        }
        fn route(&self, path: &str, route: Route) {
            self.routes.lock().unwrap().insert(path.into(), route);
        }
        fn url(&self, path: &str) -> String {
            format!("{}{path}", self.base)
        }
        fn hits(&self) -> usize {
            self.hits.lock().unwrap().len()
        }
    }

    impl Drop for Server {
        fn drop(&mut self) {
            self.server.unblock();
            if let Some(h) = self.handle.take() {
                let _ = h.join();
            }
        }
    }

    fn sha(bytes: &[u8]) -> String {
        crate::client::hex_lower(&Sha256::digest(bytes))
    }

    fn entry_for(server: &Server, id: &str, zip: &[u8]) -> CatalogEntry {
        CatalogEntry {
            id: id.into(),
            version: "1".into(),
            url: server.url(&format!("/{id}.zip")),
            sha256: sha(zip),
            bytes: zip.len() as u64,
            title: "Test place".into(),
        }
    }

    /// A zip as bytes.
    fn zip_bytes(dir: &Path, items: &[Item<'_>]) -> Vec<u8> {
        let path = dir.join("make.zip");
        make_zip(&path, items);
        fs::read(path).unwrap()
    }

    fn good_zip(dir: &Path, id: &str) -> Vec<u8> {
        let manifest = format!("{id}/manifest.json");
        let mesh = format!("{id}/mesh.bin");
        zip_bytes(
            dir,
            &[
                Item::Dir(&format!("{id}/")),
                Item::File(&manifest, b"{\"id\":\"x\"}", 0o644),
                Item::File(&mesh, &[7u8; 4096], 0o644),
            ],
        )
    }

    fn accept(_: &Path) -> std::result::Result<(), String> {
        Ok(())
    }

    fn run(
        entry: &CatalogEntry,
        places: &Path,
        validate: impl FnOnce(&Path) -> std::result::Result<(), String>,
    ) -> Result<PathBuf> {
        install_pack(
            &crate::default_agent(),
            entry,
            places,
            validate,
            |_, _| {},
            &AtomicBool::new(false),
        )
    }

    /// The folder holds nothing at all: no pack, no `.part`, no `.old`.
    fn assert_nothing_left(places: &Path) {
        let left: Vec<_> = fs::read_dir(places)
            .map(|d| d.flatten().map(|e| e.file_name()).collect())
            .unwrap_or_default();
        assert!(left.is_empty(), "left behind: {left:?}");
    }

    const SHA: &str = "8cccc3cc1fcb0770a6d22f9cea376fa0e6dc3b3bb971509ae668c500c4b73f0e";

    #[test]
    fn the_catalog_parses_and_refuses_bad_lines() {
        let good = format!(
            "# a comment\n\ncampsite 2027.1-1 https://github.com/jimas21/laterna-places/releases/download/campsite/campsite.zip {SHA} 12582912 Campsite at night\n"
        );
        let c = parse_catalog(&good, false).unwrap();
        assert_eq!(c.len(), 1);
        assert_eq!(c[0].id, "campsite");
        assert_eq!(c[0].bytes, 12_582_912);
        assert_eq!(c[0].title, "Campsite at night");
        assert_eq!(c[0].host(), "github.com");
        assert!(parse_catalog("# nothing\n", false).unwrap().is_empty());

        let line = |id: &str, url: &str, sha: &str, bytes: &str, title: &str| {
            format!("{id} 1 {url} {sha} {bytes} {title}\n")
        };
        let https = "https://github.com/x/y.zip";
        let bads = [
            line("Camp", https, SHA, "1000", "T"),
            line("../x", https, SHA, "1000", "T"),
            line("a", "http://github.com/x.zip", SHA, "1000", "T"),
            line("a", "ftp://github.com/x.zip", SHA, "1000", "T"),
            line("a", "https://u:p@github.com/x.zip", SHA, "1000", "T"),
            line("a", "http://127.0.0.1/x.zip", SHA, "1000", "T"),
            line("a", https, &SHA.to_uppercase(), "1000", "T"),
            line("a", https, "abc", "1000", "T"),
            line("a", https, SHA, "0", "T"),
            line("a", https, SHA, "41943041", "T"),
            line("a", https, SHA, "lots", "T"),
            "a 1 https://github.com/x.zip\n".to_string(),
            format!(
                "{}{}",
                line("a", https, SHA, "1000", "T"),
                line("a", https, SHA, "1000", "U")
            ),
        ];
        for bad in &bads {
            assert!(parse_catalog(bad, false).is_err(), "accepted: {bad}");
        }
        // Plain http to this machine is for tests and the preview only.
        let local = line("a", "http://127.0.0.1:9/x.zip", SHA, "1000", "T");
        assert!(parse_catalog(&local, true).is_ok());
        assert!(parse_catalog(&local, false).is_err());
        let remote = line("a", "http://example.com/x.zip", SHA, "1000", "T");
        assert!(parse_catalog(&remote, true).is_err());
    }

    #[test]
    fn a_pack_installs_and_removes_end_to_end() {
        let tmp = tempfile::tempdir().unwrap();
        let server = Server::start();
        let zip = good_zip(tmp.path(), "campsite");
        server.route("/campsite.zip", Route::Bytes(zip.clone()));
        let entry = entry_for(&server, "campsite", &zip);
        let places = tmp.path().join("places");

        assert!(!is_installed(&places, "campsite"));
        let mut last = (0, 0);
        let dest = install_pack(
            &crate::default_agent(),
            &entry,
            &places,
            |dir| {
                assert!(dir.join("manifest.json").is_file());
                Ok(())
            },
            |done, total| last = (done, total),
            &AtomicBool::new(false),
        )
        .unwrap();
        assert_eq!(dest, places.join("campsite"));
        assert_eq!(last, (zip.len() as u64, zip.len() as u64));
        assert!(is_installed(&places, "campsite"));
        assert_eq!(fs::read(dest.join("mesh.bin")).unwrap(), vec![7u8; 4096]);
        // Only the pack is in the folder: the zip and the staging are gone.
        let names: Vec<_> = fs::read_dir(&places)
            .unwrap()
            .flatten()
            .map(|e| e.file_name())
            .collect();
        assert_eq!(names, ["campsite"]);

        remove_pack(&places, "campsite").unwrap();
        assert!(!is_installed(&places, "campsite"));
        assert_nothing_left(&places);
        // Removing what is not there is fine.
        remove_pack(&places, "campsite").unwrap();
        assert!(remove_pack(&places, "../x").is_err());
    }

    #[test]
    fn a_hash_mismatch_leaves_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let server = Server::start();
        let zip = good_zip(tmp.path(), "campsite");
        let mut other = zip.clone();
        *other.last_mut().unwrap() ^= 1; // same size, different bytes
        server.route("/campsite.zip", Route::Bytes(other));
        let entry = entry_for(&server, "campsite", &zip);
        let places = tmp.path().join("places");
        let err = run(&entry, &places, accept).unwrap_err();
        assert!(matches!(err, Error::ChecksumMismatch { .. }), "{err}");
        assert_nothing_left(&places);
    }

    #[test]
    fn a_body_longer_or_shorter_than_the_catalog_says_leaves_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let server = Server::start();
        let zip = good_zip(tmp.path(), "campsite");
        let places = tmp.path().join("places");
        let entry = entry_for(&server, "campsite", &zip);

        let mut longer = zip.clone();
        longer.extend_from_slice(&[0u8; 1000]);
        server.route("/campsite.zip", Route::Bytes(longer));
        let err = run(&entry, &places, accept).unwrap_err();
        assert!(matches!(err, Error::SizeMismatch { .. }), "{err}");
        assert_nothing_left(&places);

        server.route(
            "/campsite.zip",
            Route::Bytes(zip[..zip.len() - 10].to_vec()),
        );
        let err = run(&entry, &places, accept).unwrap_err();
        assert!(matches!(err, Error::SizeMismatch { .. }), "{err}");
        assert_nothing_left(&places);
    }

    #[test]
    fn a_catalog_size_over_forty_megabytes_is_refused_before_any_request() {
        let tmp = tempfile::tempdir().unwrap();
        let server = Server::start();
        let zip = good_zip(tmp.path(), "campsite");
        let mut entry = entry_for(&server, "campsite", &zip);
        entry.bytes = MAX_PACK_BYTES + 1;
        let places = tmp.path().join("places");
        let err = run(&entry, &places, accept).unwrap_err();
        assert!(matches!(err, Error::InvalidPack(_)), "{err}");
        assert_eq!(server.hits(), 0);
        assert_nothing_left(&places);
        // An id that would climb out of the folder never gets that far.
        let mut entry = entry_for(&server, "campsite", &zip);
        entry.id = "../evil".into();
        assert!(run(&entry, &places, accept).is_err());
        assert_eq!(server.hits(), 0);
    }

    #[test]
    fn an_interrupted_download_leaves_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let server = Server::start();
        let zip = good_zip(tmp.path(), "campsite");
        // A server that promises the whole file, sends half and hangs up.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let half = zip[..zip.len() / 2].to_vec();
        let len = zip.len();
        let cutter = thread::spawn(move || {
            use std::io::{Read, Write};
            let (mut s, _) = listener.accept().unwrap();
            let mut head = [0u8; 2048];
            let _ = s.read(&mut head);
            let reply = format!("HTTP/1.1 200 OK\r\nContent-Length: {len}\r\n\r\n");
            let _ = s.write_all(reply.as_bytes());
            let _ = s.write_all(&half);
            // Dropping the stream closes the connection mid-body.
        });
        let mut entry = entry_for(&server, "campsite", &zip);
        entry.url = format!("http://127.0.0.1:{port}/campsite.zip");
        let places = tmp.path().join("places");
        assert!(run(&entry, &places, accept).is_err());
        cutter.join().unwrap();
        assert_nothing_left(&places);
        // Then the connection comes back: the same pack installs.
        let entry = entry_for(&server, "campsite", &zip);
        server.route("/campsite.zip", Route::Bytes(zip));
        run(&entry, &places, accept).unwrap();
        assert!(is_installed(&places, "campsite"));
    }

    #[test]
    fn a_file_name_in_the_url_cannot_collide_with_the_work_folders() {
        let tmp = tempfile::tempdir().unwrap();
        let server = Server::start();
        let zip = good_zip(tmp.path(), "campsite");
        for name in ["pack", "dl"] {
            server.route(&format!("/{name}"), Route::Bytes(zip.clone()));
            let mut entry = entry_for(&server, "campsite", &zip);
            entry.url = server.url(&format!("/{name}"));
            let places = tmp.path().join(format!("places-{name}"));
            run(&entry, &places, accept).unwrap();
            assert!(is_installed(&places, "campsite"), "{name}");
        }
    }

    #[test]
    fn a_folder_of_the_persons_own_is_never_replaced_or_removed() {
        let tmp = tempfile::tempdir().unwrap();
        let server = Server::start();
        let zip = good_zip(tmp.path(), "campsite");
        server.route("/campsite.zip", Route::Bytes(zip.clone()));
        let entry = entry_for(&server, "campsite", &zip);
        let places = tmp.path().join("environments");
        // Their own pack, made by hand under the name the entry has.
        let own = places.join("campsite");
        fs::create_dir_all(&own).unwrap();
        fs::write(own.join("manifest.json"), b"{\"id\":\"mine\"}").unwrap();
        fs::write(own.join("mesh.bin"), b"theirs").unwrap();
        assert!(is_installed(&places, "campsite"));
        assert!(!is_installed_by_app(&places, "campsite"));
        assert!(is_taken_by_other(&places, "campsite"));

        // Refused before anything is downloaded, and nothing is touched.
        let r = install_pack_in(
            &crate::default_agent(),
            &entry,
            &places,
            true,
            accept,
            |_, _| {},
            &AtomicBool::new(false),
        );
        assert!(matches!(r, Err(Error::InvalidPack(_))), "{r:?}");
        assert_eq!(server.hits(), 0, "it downloaded over their folder");
        assert_eq!(fs::read(own.join("mesh.bin")).unwrap(), b"theirs");
        // Nor is it removed.
        assert!(remove_installed_pack(&places, "campsite").is_err());
        assert_eq!(fs::read(own.join("mesh.bin")).unwrap(), b"theirs");

        // Where nothing is in the way it installs, marked, and a second
        // install (an update) replaces it, and Remove deletes it.
        fs::remove_dir_all(&own).unwrap();
        for _ in 0..2 {
            install_pack_in(
                &crate::default_agent(),
                &entry,
                &places,
                true,
                accept,
                |_, _| {},
                &AtomicBool::new(false),
            )
            .unwrap();
            assert!(is_installed_by_app(&places, "campsite"));
            assert!(!is_taken_by_other(&places, "campsite"));
        }
        remove_installed_pack(&places, "campsite").unwrap();
        assert!(!is_installed(&places, "campsite"));
        assert_nothing_left(&places);
    }

    #[test]
    fn cancelling_leaves_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let server = Server::start();
        let zip = good_zip(tmp.path(), "campsite");
        server.route("/campsite.zip", Route::Bytes(zip.clone()));
        let entry = entry_for(&server, "campsite", &zip);
        let places = tmp.path().join("places");
        let cancel = AtomicBool::new(true);
        let err = install_pack(
            &crate::default_agent(),
            &entry,
            &places,
            accept,
            |_, _| {},
            &cancel,
        )
        .unwrap_err();
        assert!(matches!(err, Error::Cancelled), "{err}");
        assert_nothing_left(&places);
    }

    #[test]
    fn unsafe_archives_leave_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let places = tmp.path().join("places");
        let cases: Vec<(&str, Vec<Item<'_>>)> = vec![
            (
                "parent traversal",
                vec![
                    Item::File("campsite/manifest.json", b"{}", 0o644),
                    Item::File("campsite/../escape.txt", b"x", 0o644),
                ],
            ),
            (
                "absolute path",
                vec![
                    Item::File("campsite/manifest.json", b"{}", 0o644),
                    Item::File("/etc/escape.txt", b"x", 0o644),
                ],
            ),
            (
                "outside the id folder",
                vec![
                    Item::File("campsite/manifest.json", b"{}", 0o644),
                    Item::File("other/file.bin", b"x", 0o644),
                ],
            ),
            (
                "a symbolic link",
                vec![
                    Item::File("campsite/manifest.json", b"{}", 0o644),
                    Item::Symlink("campsite/link", "/etc/passwd"),
                ],
            ),
            (
                "no manifest",
                vec![Item::File("campsite/mesh.bin", b"x", 0o644)],
            ),
            ("empty", vec![]),
        ];
        for (what, items) in cases {
            let server = Server::start();
            let zip = zip_bytes(tmp.path(), &items);
            server.route("/campsite.zip", Route::Bytes(zip.clone()));
            let entry = entry_for(&server, "campsite", &zip);
            let r = run(&entry, &places, accept);
            assert!(r.is_err(), "{what} was accepted");
            assert_nothing_left(&places);
            assert!(!tmp.path().join("escape.txt").exists(), "{what}");
        }
    }

    #[test]
    fn a_zip_that_holds_more_than_it_declares_is_stopped() {
        // The unpacked cap counts bytes as they are written.
        let tmp = tempfile::tempdir().unwrap();
        let zip = zip_bytes(
            tmp.path(),
            &[
                Item::File("campsite/manifest.json", b"{}", 0o644),
                Item::File("campsite/big.bin", &vec![0u8; 2 << 20], 0o644),
            ],
        );
        let path = tmp.path().join("p.zip");
        fs::write(&path, &zip).unwrap();
        let err = extract_zip_capped(&path, &tmp.path().join("out"), 1 << 20).unwrap_err();
        assert!(matches!(err, Error::ArchiveTooLarge(_)), "{err}");
    }

    #[test]
    fn a_three_seat_pack_fits_the_unpacked_cap_and_a_bigger_one_does_not() {
        // A format 3 pack with three full sets (32 MiB of textures each, and a
        // mesh) must install; anything past the cap is refused before unpacking.
        let tmp = tempfile::tempdir().unwrap();
        let set = vec![0u8; 32 << 20];
        let mesh = vec![0u8; 5 << 20];
        let fits = zip_bytes(
            tmp.path(),
            &[
                Item::File("multiplex/manifest.json", b"{}", 0o644),
                Item::File("multiplex/front-house.ktx2", &set, 0o644),
                Item::File("multiplex/middle-house.ktx2", &set, 0o644),
                Item::File("multiplex/back-house.ktx2", &set, 0o644),
                Item::File("multiplex/front-mesh.bin", &mesh, 0o644),
                Item::File("multiplex/middle-mesh.bin", &mesh, 0o644),
                Item::File("multiplex/back-mesh.bin", &mesh, 0o644),
            ],
        );
        let path = tmp.path().join("fits.zip");
        fs::write(&path, &fits).unwrap();
        inspect(&path, "multiplex").unwrap();

        let over = vec![0u8; (MAX_UNPACKED_BYTES - (96 << 20)) as usize + 1];
        let too_big = zip_bytes(
            tmp.path(),
            &[
                Item::File("multiplex/manifest.json", b"{}", 0o644),
                Item::File("multiplex/front-house.ktx2", &set, 0o644),
                Item::File("multiplex/middle-house.ktx2", &set, 0o644),
                Item::File("multiplex/back-house.ktx2", &set, 0o644),
                Item::File("multiplex/extra.bin", &over, 0o644),
            ],
        );
        let path = tmp.path().join("over.zip");
        fs::write(&path, &too_big).unwrap();
        let err = inspect(&path, "multiplex").unwrap_err();
        assert!(matches!(err, Error::ArchiveTooLarge(_)), "{err}");
    }

    #[test]
    fn a_pack_the_apps_checks_refuse_leaves_nothing_and_keeps_the_old_one() {
        let tmp = tempfile::tempdir().unwrap();
        let server = Server::start();
        let zip = good_zip(tmp.path(), "campsite");
        server.route("/campsite.zip", Route::Bytes(zip.clone()));
        let entry = entry_for(&server, "campsite", &zip);
        let places = tmp.path().join("places");

        // Nothing installed: a refusal leaves nothing.
        let err = run(&entry, &places, |_| Err("the mesh is wrong".into())).unwrap_err();
        assert!(
            matches!(&err, Error::InvalidPack(m) if m == "the mesh is wrong"),
            "{err}"
        );
        assert_nothing_left(&places);

        // An older install of the same pack survives a refused update.
        run(&entry, &places, accept).unwrap();
        fs::write(places.join("campsite").join("mark"), b"old").unwrap();
        assert!(run(&entry, &places, |_| Err("no".into())).is_err());
        assert_eq!(fs::read(places.join("campsite/mark")).unwrap(), b"old");
        assert!(!places.join(".campsite.part").exists());
        assert!(!places.join(".campsite.old").exists());

        // An accepted update replaces it whole.
        run(&entry, &places, accept).unwrap();
        assert!(!places.join("campsite/mark").exists());
        assert!(places.join("campsite/manifest.json").is_file());
        assert!(!places.join(".campsite.old").exists());
    }

    #[test]
    fn the_https_only_agent_refuses_plain_http_before_connecting() {
        // ureq 3.5 checks the scheme of every hop (run.rs `call_run`, which
        // the redirect loop calls again for each Location), so a redirect to
        // plain http is refused the same way. A real redirect needs an https
        // server first, which these tests do not have: this covers the
        // check itself, and that nothing is sent.
        let tmp = tempfile::tempdir().unwrap();
        let server = Server::start();
        let zip = good_zip(tmp.path(), "campsite");
        server.route("/campsite.zip", Route::Bytes(zip.clone()));
        let entry = entry_for(&server, "campsite", &zip);
        let places = tmp.path().join("places");
        let r = install_pack(
            &crate::https_only_agent(),
            &entry,
            &places,
            accept,
            |_, _| {},
            &AtomicBool::new(false),
        );
        assert!(r.is_err());
        assert_eq!(server.hits(), 0, "the agent connected");
        assert_nothing_left(&places);
    }

    #[test]
    fn the_default_agent_follows_a_redirect_which_is_what_the_https_check_guards() {
        let tmp = tempfile::tempdir().unwrap();
        let server = Server::start();
        let zip = good_zip(tmp.path(), "campsite");
        server.route("/real.zip", Route::Bytes(zip.clone()));
        server.route("/campsite.zip", Route::Redirect(server.url("/real.zip")));
        let entry = entry_for(&server, "campsite", &zip);
        run(&entry, &tmp.path().join("places"), accept).unwrap();
        assert_eq!(server.hits(), 2);
    }

    #[test]
    fn only_a_catalog_wholly_on_this_machine_may_use_plain_http() {
        let entry = |url: &str| CatalogEntry {
            id: "a".into(),
            version: "1".into(),
            url: url.into(),
            sha256: "0".repeat(64),
            bytes: 1,
            title: "A".into(),
        };
        let local = entry("http://127.0.0.1:9/a.zip");
        let v6 = entry("http://[::1]:9/a.zip");
        let name = entry("http://localhost:9/a.zip");
        let remote = entry("https://github.com/x/a.zip");
        assert!(!needs_https_only(&[local.clone(), v6, name]));
        assert!(needs_https_only(&[]));
        assert!(needs_https_only(std::slice::from_ref(&remote)));
        assert!(needs_https_only(&[local, remote]));
    }

    #[test]
    fn clean_up_removes_cut_off_installs_and_restores_a_replaced_pack() {
        let tmp = tempfile::tempdir().unwrap();
        let places = tmp.path();
        fs::create_dir_all(places.join(".a.part/unpacked")).unwrap();
        fs::write(places.join(".a.part/x.zip"), b"half").unwrap();
        // b was moved aside and its new copy never arrived: it comes back.
        fs::create_dir_all(places.join(".b.old")).unwrap();
        fs::write(places.join(".b.old/manifest.json"), b"{}").unwrap();
        // c has its new copy: the old one is just removed.
        fs::create_dir_all(places.join(".c.old")).unwrap();
        fs::create_dir_all(places.join("c")).unwrap();
        // An unrelated folder and an ordinary pack are untouched.
        fs::create_dir_all(places.join("d")).unwrap();
        clean_up(places);
        let mut names: Vec<String> = fs::read_dir(places)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        assert_eq!(names, ["b", "c", "d"]);
        assert!(places.join("b/manifest.json").is_file());
    }

    #[test]
    fn a_source_address_must_be_a_public_https_one() {
        assert_eq!(
            check_source_url("  https://example.com/places.lock \n").unwrap(),
            "https://example.com/places.lock"
        );
        for bad in [
            "",
            "http://example.com/a",
            "https://user:pw@example.com/a",
            "https://localhost/a",
            "https://10.0.0.5/a",
            "https://nas.local/a",
            "https://example.com/a b",
            "file:///etc/passwd",
            "example.com",
        ] {
            assert!(check_source_url(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn an_index_keeps_its_good_lines_and_counts_the_rest() {
        let good = |id: &str| format!("{id} 1 https://example.com/{id}.zip {SHA} 1000 Pack {id}\n");
        let text = format!(
            "# mine\n{}two 1 http://example.com/two.zip {SHA} 1000 Plain http\nbad line\n{}three 1 https://127.0.0.1/t.zip {SHA} 1000 Loopback\n{}",
            good("one"),
            good("one"),
            good("four"),
        );
        let ix = parse_index(&text);
        let ids: Vec<&str> = ix.entries.iter().map(|e| e.id.as_str()).collect();
        assert_eq!(ids, ["one", "four"]);
        // http, bad line, duplicate, loopback.
        assert_eq!(ix.skipped, 4);
        assert_eq!(parse_index("# nothing\n"), Index::default());
    }

    #[test]
    fn an_index_offers_at_most_fifty_packs() {
        let text: String = (0..60)
            .map(|n| format!("p{n} 1 https://example.com/p{n}.zip {SHA} 1000 P{n}\n"))
            .collect();
        let ix = parse_index(&text);
        assert_eq!(ix.entries.len(), MAX_INDEX_ENTRIES);
        assert_eq!(ix.skipped, 10);
    }

    #[test]
    fn an_index_is_fetched_with_a_size_cap() {
        let server = Server::start();
        let line = format!("one 1 https://example.com/one.zip {SHA} 1000 One\n");
        server.route("/index", Route::Bytes(line.into_bytes()));
        server.route(
            "/huge",
            Route::Bytes(vec![b'#'; MAX_INDEX_BYTES as usize + 10]),
        );
        let agent = crate::default_agent();
        let ix = fetch_index(&agent, &server.url("/index")).unwrap();
        assert_eq!(ix.entries.len(), 1);
        assert!(fetch_index(&agent, &server.url("/huge")).is_err());
        assert!(matches!(
            fetch_index(&agent, &server.url("/none")),
            Err(Error::HttpStatus { status: 404, .. })
        ));
        // The app's agent never fetches plain http.
        assert!(fetch_index(&crate::https_only_agent(), &server.url("/index")).is_err());
    }
}
