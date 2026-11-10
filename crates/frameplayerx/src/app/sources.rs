//! Browsing sources: folders, scene feeds, server catalogs, and PIN locks.

use super::*;

fn request_source(library: &Library, req: &OpenRequest) -> Option<String> {
    fp_sources::location_owner(&req.location)
        .or_else(|| req.source_id.clone())
        .or_else(|| {
            library
                .get_by_location(&req.location)
                .ok()
                .flatten()
                .map(|r| r.source_id)
        })
}

impl App {
    /// Whether source `id` has a PIN and is not unlocked.
    pub(super) fn source_locked(&self, id: &str) -> bool {
        crate::pin::locked(&self.settings.source_pins, &self.ui.unlocked_sources, id)
    }

    /// The media library.
    pub(crate) fn library(&self) -> &Library {
        &self.services.library
    }

    pub(crate) fn source_config(&self, id: &str) -> Option<&fp_sources::SourceConfig> {
        self.services.source_config(id)
    }

    pub(crate) fn source_name(&self, id: &str) -> String {
        self.services
            .source_config(id)
            .map(|c| c.name().to_string())
            .unwrap_or_else(|| id.to_string())
    }

    /// Asks for the PIN of a locked source.
    pub(super) fn ask_pin(&mut self, source: &str) {
        self.ui.source = None;
        self.ui.screen = Screen::Sources;
        self.show_browser = true;
        self.ui_visible = true;
        self.ui.pin_form = Some(ui::PinForm {
            source: source.to_string(),
            name: self.source_name(source),
            ..Default::default()
        });
    }

    /// The locked source a video belongs to, if any: the one it was opened
    /// from, else the one its library entry came from.
    pub(super) fn locked_source_of(&self, req: &OpenRequest) -> Option<String> {
        if self.settings.source_pins.is_empty() {
            return None;
        }
        let source = request_source(&self.services.library, req)?;
        self.source_locked(&source).then_some(source)
    }

    /// What locked sources hide: their videos.
    pub(super) fn locked_hidden(&self) -> fp_library::Hidden {
        let mut sources =
            crate::pin::locked_ids(&self.settings.source_pins, &self.ui.unlocked_sources);
        // Plex profiles that are not the one watching are left out as well.
        sources.extend(crate::plex_profiles::hidden(
            &self.services.source_configs,
            &self.settings.plex_profiles,
        ));
        fp_library::Hidden { sources }
    }

    /// Leaves locked sources' videos, and those of Plex profiles that are not
    /// watching, out of the library's listings (and the remotes'); nothing
    /// changes while no source has a PIN or a profile.
    pub(super) fn sync_locks(&mut self) {
        if self.settings.source_pins.is_empty()
            && self.services.library.hidden().is_empty()
            && !crate::plex_profiles::any(&self.services.source_configs)
        {
            return;
        }
        let hidden = self.locked_hidden();
        if hidden != self.services.library.hidden() {
            // What is playing stops when its source is hidden.
            let stops = self.playback.as_ref().is_some_and(|p| {
                request_source(&self.services.library, &p.request)
                    .is_some_and(|source| hidden.sources.contains(&source))
            });
            self.services.library.set_hidden(hidden);
            self.ui.invalidate();
            if stops {
                self.close_playback();
            }
        }
    }

    pub fn browse(&mut self, source: String, location: Option<String>) {
        if self.source_locked(&source) {
            self.ask_pin(&source);
            return;
        }
        let Some(src) = self.services.opener.source(&source) else {
            self.ui.toast(tr!("source-not-available"));
            return;
        };
        // Into the listing on screen, else a folder page of its own.
        if !self.ui.folder_mut().is_some_and(|b| b.source == source) {
            self.ui.source = Some(SourceView {
                source: source.clone(),
                name: src.name().to_string(),
                back_to: Screen::Sources,
                page: SourcePage::Folders(Browse {
                    source: source.clone(),
                    ..Default::default()
                }),
            });
        }
        let Some(b) = self.ui.folder_mut() else {
            return;
        };
        b.location = location.clone();
        b.loading = true;
        b.error = None;
        let lib = self.services.library.clone();
        self.jobs.spawn("list", move || {
            let result = src.list(location.as_deref()).map(|mut e| {
                // Browsing can teach feeds and DLNA the stable IDs behind
                // legacy media URLs. Convert those rows before looking them up.
                if matches!(
                    src.kind(),
                    fp_sources::SourceKind::Dlna
                        | fp_sources::SourceKind::DeoVr
                        | fp_sources::SourceKind::HereSphere
                ) && let Err(error) = lib
                    .rewrite_source_locations(&source, |location| src.canonical_location(location))
                {
                    log::warn!("cannot update saved media locations for source {source}: {error}");
                }
                // The library's pictures for these videos: where one was
                // left off, or a thumbnail made for one the server has none
                // for.
                for entry in e
                    .iter_mut()
                    .filter(|e| e.kind == fp_core::source::EntryKind::Video)
                {
                    if let Ok(Some(r)) = lib.get_by_location(&entry.location)
                        && (r.partly_watched() || entry.thumbnail_url.is_none())
                        && let Some(p) = r.picture()
                    {
                        entry.thumbnail_url = Some(p);
                    }
                }
                e
            });
            let result = result.map_err(|e| {
                let removable = source == crate::services::REMOVABLE_SOURCE;
                match e {
                    fp_sources::Error::NotInserted => tr!("source-not-inserted").to_string(),
                    _ if removable && crate::drives::removable_mounts().is_empty() => {
                        tr!("source-no-drives").to_string()
                    }
                    e => ui::errors::source(&e),
                }
            });
            Job::Listed {
                source,
                location,
                result,
            }
        });
    }

    /// Opens a source in the browser its kind gets: the headset's videos
    /// and folders, a Plex or Jellyfin/Emby client, a feed's scenes, or folders.
    pub(super) fn open_source(&mut self, source: String, back_to: Screen) {
        self.ui.details = None;
        self.ui.pin_form = None;
        self.ui.source_form = None;
        self.ui.screen = Screen::Sources;
        if source == crate::services::LOCAL_HUB {
            self.ui.source = Some(SourceView {
                source,
                name: tr!("sources-local").into(),
                back_to,
                page: SourcePage::Local(Default::default()),
            });
            return;
        }
        // A profile that is not watching has no page: its server's page for
        // the one that is, if it has one.
        let source = match crate::plex_profiles::counterpart(
            &self.services.source_configs,
            &self.settings.plex_profiles,
            &source,
        ) {
            Some(active) => active,
            None if crate::plex_profiles::hidden(
                &self.services.source_configs,
                &self.settings.plex_profiles,
            )
            .contains(&source) =>
            {
                self.ui.toast(tr!("plex-profile-not-shared-here"));
                return;
            }
            None => source,
        };
        if self.source_locked(&source) {
            self.ask_pin(&source);
            return;
        }
        let Some(kind) = self
            .services
            .source_config(&source)
            .map(fp_sources::SourceConfig::kind)
        else {
            self.ui.toast(tr!("source-not-available"));
            return;
        };
        let name = self.source_name(&source);
        self.feed_queue.clear();
        let view = |page| SourceView {
            source: source.clone(),
            name: name.clone(),
            back_to,
            page,
        };
        match kind {
            fp_sources::SourceKind::MediaBrowser | fp_sources::SourceKind::Plex => {
                // A server that was asleep when its pictures failed may
                // answer now.
                self.thumbs.retry_failed_remote();
                let seq = self.ui.next_seq();
                let cached = self.ui.catalog_cache.get(&source);
                let j = ui::catalog::CatalogView::new(seq, cached);
                self.ui.source = Some(view(SourcePage::Catalog(j)));
                self.apply(Action::Catalog {
                    source,
                    seq,
                    load: ui::catalog::Load::Home,
                });
            }
            fp_sources::SourceKind::DeoVr | fp_sources::SourceKind::HereSphere => {
                let f = ui::feeds::FeedView::new(kind);
                self.ui.source = Some(view(SourcePage::Feed(f)));
                self.apply(Action::FeedLoad { source });
            }
            fp_sources::SourceKind::Library => {
                let l = ui::local::LibraryView::new(&source);
                self.ui.source = Some(view(SourcePage::Library(l)));
                // New files show up without a restart.
                self.scan_libraries(|l| l.id == source, false, false);
            }
            _ => {
                self.ui.source = Some(view(SourcePage::Folders(Browse {
                    source: source.clone(),
                    ..Default::default()
                })));
                self.browse(source, None);
            }
        }
    }

    /// Fetches waiting feed scenes' details, a few at a time.
    pub(super) fn pump_feed(&mut self) {
        const WORKERS: usize = 6;
        let current = match self.ui.source.as_ref() {
            Some(SourceView {
                source,
                page: SourcePage::Feed(_),
                ..
            }) => source.clone(),
            _ => {
                self.feed_queue.clear();
                return;
            }
        };
        while self.feed_busy < WORKERS {
            let Some((source, url)) = self.feed_queue.pop_front() else {
                break;
            };
            if source != current {
                continue;
            }
            let Some(src) = self.services.opener.source(&source) else {
                continue;
            };
            self.feed_busy += 1;
            self.jobs.spawn("feed-scene", move || {
                let result = scene_details(src.as_ref(), &url, false);
                Job::FeedScene {
                    source,
                    url,
                    full: false,
                    result: Box::new(result),
                }
            });
        }
    }

    /// Brings a catalog page up to date after watching (marks, resume
    /// points and "next up" changed on the server). Unless `force`, a page
    /// the server answered a moment ago stays as it is.
    pub(super) fn refresh_source(&mut self, force: bool) {
        let Some(SourceView {
            source,
            page: SourcePage::Catalog(j),
            ..
        }) = self.ui.source.as_mut()
        else {
            return;
        };
        let source = source.clone();
        if !force && j.loaded_recently() {
            return;
        }
        self.thumbs.retry_failed_remote();
        if let Some((seq, load)) = j.reload() {
            // After the server has heard that playback stopped.
            self.catalog_job(source, seq, load, Duration::from_millis(800));
        }
    }

    /// Asks a media server for a catalog page, after `delay`.
    pub(super) fn catalog_job(
        &mut self,
        source: String,
        seq: u64,
        load: ui::catalog::Load,
        delay: Duration,
    ) {
        let Some(src) = self.services.opener.source(&source) else {
            return;
        };
        let request_seq = self.ui.next_seq();
        let Some(seq) = self
            .ui
            .catalog_page(&source)
            .and_then(|page| page.begin_request(seq, request_seq, &load))
        else {
            return;
        };
        self.jobs.spawn("catalog", move || {
            std::thread::sleep(delay);
            let result = match src.as_catalog() {
                Some(mb) => ui::catalog::run(mb, &load).map_err(|e| ui::errors::source(&e)),
                None => Err(tr!("source-not-available").to_string()),
            };
            Job::Catalog {
                source,
                seq,
                result: Box::new(result),
            }
        });
    }

    /// Scans library `source` now, saying how it went; `force` makes its
    /// thumbnails again.
    pub(super) fn rescan(&mut self, source: &str, force: bool) {
        let Some(fp_sources::SourceConfig::Library(l)) = self.source_config(source) else {
            return;
        };
        if !crate::libraries::present(l) {
            let card = l.card.as_ref().map_or("", |c| c.label.as_str());
            self.ui
                .toast(tr!("scan-insert-card", card = card, name = l.name.as_str()));
            return;
        }
        if self.ui.scans.contains_key(source) {
            self.ui.toast(tr!("scan-busy", name = l.name.as_str()));
            return;
        }
        self.scan_libraries(|l| l.id == source, force, true);
    }

    // ---- editing sources ----

    pub(super) fn add_source(&mut self, cfg: fp_sources::SourceConfig) {
        let change = self.add_sources(vec![cfg.clone()]);
        match change.errors.into_iter().find(|e| e.id == cfg.id()) {
            Some(e) => self.ui.toast(e.to_string()),
            None => {
                self.ui.toast(match change.save_error {
                    Some(e) => tr!("source-added-unsaved", name = cfg.name(), error = e),
                    None => tr!("source-added", name = cfg.name()),
                });
                self.open_source(cfg.id().to_string(), Screen::Sources);
            }
        }
    }

    /// Adds sources together: one save, and only they are built. One whose
    /// id is already in the list takes its place.
    pub(super) fn add_sources(
        &mut self,
        new: Vec<fp_sources::SourceConfig>,
    ) -> crate::services::SourcesChange {
        use std::collections::HashSet;
        let ids: HashSet<String> = new.iter().map(|c| c.id().to_string()).collect();
        // A source that is replaced may be another profile's or server's now.
        for id in &ids {
            self.ui.catalog_cache.forget(id);
        }
        let change = self.services.replace_sources(|list| {
            for cfg in new {
                match list.iter_mut().find(|c| c.id() == cfg.id()) {
                    Some(slot) => *slot = cfg,
                    None => list.push(cfg),
                }
            }
            ids
        });
        self.ui.home = None;
        change
    }

    pub(super) fn update_source(&mut self, cfg: fp_sources::SourceConfig) {
        let id = cfg.id().to_string();
        // The form only opens for unlocked sources, but a PIN could have
        // been set since.
        if self.source_locked(&id) {
            self.ask_pin(&id);
            return;
        }
        let moved = match (self.source_config(&id), &cfg) {
            (
                Some(fp_sources::SourceConfig::Library(old)),
                fp_sources::SourceConfig::Library(new),
            ) => old.root() != new.root(),
            _ => false,
        };
        let change = self.services.replace_sources(|list| {
            if let Some(slot) = list.iter_mut().find(|c| c.id() == id) {
                *slot = cfg.clone();
            }
            std::collections::HashSet::from([id.clone()])
        });
        if moved {
            // Its next scan takes back the videos still in the new folder.
            let lib = &self.services.library;
            if let Err(e) = lib
                .forget_unwatched(&id)
                .and_then(|_| lib.detach_source(&id))
            {
                log::warn!("letting go of the videos of {id}: {e}");
            }
            self.scan_libraries(|l| l.id == id, false, false);
        }
        match change.errors.into_iter().find(|e| e.id == id) {
            Some(e) => self.ui.toast(e.to_string()),
            None => self.ui.toast(match change.save_error {
                Some(e) => tr!("source-changed-unsaved", name = cfg.name(), error = e),
                None => tr!("source-saved", name = cfg.name()),
            }),
        }
        // Its browser and saved listing were made with the old details.
        self.close_source_view(&id);
        self.ui.catalog_cache.forget(&id);
        let _ = std::fs::remove_file(fp_sources::FeedLibrary::cache_path(&id));
        self.ui.home = None;
    }

    pub(super) fn remove_source(&mut self, id: &str) {
        self.remove_sources(&[id.to_string()]);
    }

    /// Removes sources together: one save and one rebuild, and nothing at
    /// all while any of them is locked (the first is asked for its PIN).
    /// A source's PIN goes with it, so one added again is not locked. True
    /// when they were removed and the list was saved.
    pub(super) fn remove_sources(&mut self, ids: &[String]) -> bool {
        if let Some(locked) = ids.iter().find(|id| self.source_locked(id)) {
            let locked = locked.clone();
            self.ask_pin(&locked);
            return false;
        }
        for id in ids {
            self.settings.source_pins.remove(id);
            self.ui.unlocked_sources.remove(id);
            self.close_source_view(id);
            self.ui.catalog_cache.forget(id);
            let _ = std::fs::remove_file(fp_sources::FeedLibrary::cache_path(id));
            // Its videos go with it, except what was watched or marked.
            match self.services.library.forget_unwatched(id) {
                Ok(n) if n > 0 => log::info!("forgot {n} unwatched videos of {id}"),
                Ok(_) => {}
                Err(e) => log::warn!("forgetting the videos of {id}: {e}"),
            }
        }
        // The form of a source that is gone has nothing to save to.
        let editing = |f: &ui::SourceForm| {
            f.editing
                .as_ref()
                .is_some_and(|c| ids.iter().any(|id| id == c.id()))
        };
        if self.ui.source_form.as_ref().is_some_and(editing) {
            self.ui.source_form = None;
        }
        self.ui.invalidate();
        let change = self.services.replace_sources(|list| {
            list.retain(|c| !ids.iter().any(|id| id == c.id()));
            std::collections::HashSet::new()
        });
        match change.save_error {
            Some(e) => {
                self.ui.toast(tr!("source-removed-unsaved", error = e));
                false
            }
            None => true,
        }
    }

    /// Leaves source `id`'s browser if it is open.
    fn close_source_view(&mut self, id: &str) {
        if self.ui.source.as_ref().is_some_and(|s| s.source == id) {
            self.ui.source = None;
        }
    }

    // ---- PINs ----

    pub(super) fn unlock_source(&mut self, source: String, pin: &str) {
        let right = self
            .settings
            .source_pins
            .get(&source)
            .is_some_and(|h| crate::pin::verify(h, pin));
        if right {
            self.ui.unlocked_sources.insert(source.clone());
            self.ui.pin_form = None;
            self.sync_locks();
            self.open_source(source, Screen::Sources);
        } else if let Some(f) = self.ui.pin_form.as_mut() {
            f.pin.clear();
            f.wrong = true;
        }
    }

    pub(super) fn lock_source(&mut self, id: &str) {
        self.ui.unlocked_sources.remove(id);
        self.close_source_view(id);
        let name = self.source_name(id);
        self.ui.toast(tr!("source-locked-toast", name = name));
    }

    /// Gives a source a PIN, or takes it away (`None`).
    pub(super) fn set_source_pin(&mut self, source: &str, pin: Option<String>) {
        // Changing it needs the source open, as the PIN form only offers it
        // then.
        if self.source_locked(source) {
            self.ask_pin(source);
            return;
        }
        self.ui.pin_form = None;
        let name = self.source_name(source);
        match pin {
            Some(pin) if crate::pin::valid(&pin) => {
                self.settings
                    .source_pins
                    .insert(source.to_string(), crate::pin::hash(&pin));
                self.ui.unlocked_sources.remove(source);
                self.close_source_view(source);
                // Locked now: its details are not for showing.
                let editing =
                    |f: &ui::SourceForm| f.editing.as_ref().is_some_and(|c| c.id() == source);
                if self.ui.source_form.as_ref().is_some_and(editing) {
                    self.ui.source_form = None;
                }
                self.ui.toast(tr!("source-pin-set", name = name.as_str()));
            }
            Some(_) => {}
            None => {
                self.settings.source_pins.remove(source);
                self.ui.unlocked_sources.remove(source);
                self.ui
                    .toast(tr!("source-pin-removed", name = name.as_str()));
            }
        }
    }

    // ---- Catalogs, feeds, DLNA ----

    /// Marks a catalog item as a favorite, or as watched, on the
    /// server.
    pub(super) fn mark_catalog(
        &mut self,
        source: &str,
        id: String,
        mark: ui::catalog::Mark,
        on: bool,
    ) {
        let Some(src) = self.services.opener.source(source) else {
            return;
        };
        self.jobs.spawn("catalog-mark", move || {
            let Some(mb) = src.as_catalog() else {
                return Job::Toast(tr!("source-not-available").into());
            };
            let r = match mark {
                ui::catalog::Mark::Favorite => mb.set_favorite(&id, on),
                ui::catalog::Mark::Played => mb.set_played(&id, on),
            };
            match r {
                Ok(()) => Job::Done,
                Err(e) => Job::Toast(tr!("server-refused", error = ui::errors::source(&e))),
            }
        });
    }

    /// Marks a HereSphere scene as a favourite on the server, or unmarks it.
    pub(super) fn mark_feed_favorite(&mut self, source: &str, url: String, on: bool) {
        let Some(src) = self.services.opener.source(source) else {
            return;
        };
        self.jobs.spawn("feed-favorite", move || {
            let Some(feed) = src.as_scene_feed() else {
                return Job::Toast(tr!("source-not-feed").into());
            };
            match feed.set_favorite(&url, on) {
                Ok(()) => Job::Done,
                Err(e) => Job::Toast(tr!("server-refused", error = ui::errors::source(&e))),
            }
        });
    }

    /// Lists a feed's scenes: the last listing at once, then the server's,
    /// then (HereSphere) its scan.
    pub(super) fn load_feed(&mut self, source: String) {
        let Some(src) = self.services.opener.source(&source) else {
            return;
        };
        let tx = self.jobs.sender();
        self.jobs.spawn("feed", move || {
            let path = fp_sources::FeedLibrary::cache_path(&source);
            let listed =
                |result: Result<fp_sources::FeedLibrary, String>, cached| Job::FeedLibrary {
                    source: source.clone(),
                    result,
                    cached,
                };
            // A DeoVR feed's root can take seconds.
            let saved = fp_sources::FeedLibrary::load(&path);
            if let Some(lib) = &saved {
                let _ = tx.send(listed(Ok(lib.clone()), true));
            }
            let Some(feed) = src.as_scene_feed() else {
                return listed(Err(tr!("source-not-scene-feed").into()), false);
            };
            let mut lib = match feed.library() {
                Ok(lib) => lib,
                Err(e) => return listed(Err(ui::errors::source(&e)), false),
            };
            let _ = tx.send(listed(Ok(lib.clone()), false));
            let scenes = feed.scan();
            if let Some(s) = &scenes {
                lib.merge_scan(s.clone());
            }
            if let Some(saved) = &saved {
                lib.keep_details(saved);
            }
            if let Err(e) = lib.save(&path) {
                log::warn!("saving the listing of {source}: {e}");
            }
            Job::FeedScan {
                source: source.clone(),
                scenes,
            }
        });
    }

    /// Queues feed scenes for their details: `front` (cards on screen)
    /// before those already waiting.
    pub(super) fn queue_feed_scenes(&mut self, source: &str, urls: Vec<String>, front: bool) {
        let items = urls.into_iter().map(|u| (source.to_string(), u));
        if front {
            for item in items.rev() {
                self.feed_queue.push_front(item);
            }
        } else {
            self.feed_queue.extend(items);
        }
        self.pump_feed();
    }

    /// Fetches everything about one scene, for its page.
    pub(super) fn load_feed_scene(&mut self, source: String, url: String) {
        let Some(src) = self.services.opener.source(&source) else {
            return;
        };
        self.jobs.spawn("feed-scene", move || {
            let result = scene_details(src.as_ref(), &url, true);
            Job::FeedScene {
                source,
                url,
                full: true,
                result: Box::new(result),
            }
        });
    }

    pub(super) fn discover_dlna(&mut self) {
        self.ui.dlna_searching = true;
        self.jobs.spawn("dlna", || {
            Job::Dlna(
                fp_sources::discover(Duration::from_secs(3)).map_err(|e| ui::errors::source(&e)),
            )
        });
    }

    // ---- Quick Connect ----

    pub(super) fn quick_connect_initiate(&mut self, address: String, insecure_tls: bool) {
        let seq = self.ui.next_seq();
        self.quick_connect_request = seq;
        self.jobs.spawn("quick_connect_initiate", move || {
            let addr = fp_sources::urlutil::with_http_scheme(&address);
            Job::QuickConnectInitiated(
                seq,
                fp_sources::quick_connect_initiate_url(&addr, insecure_tls)
                    .map_err(|e| ui::errors::source(&e)),
            )
        });
    }

    pub(super) fn quick_connect_poll(
        &mut self,
        address: String,
        secret: String,
        insecure_tls: bool,
    ) {
        self.jobs.spawn("quick_connect_poll", move || {
            let addr = fp_sources::urlutil::with_http_scheme(&address);
            let r = fp_sources::quick_connect_poll_url(&addr, &secret, insecure_tls)
                .map_err(|e| ui::errors::source(&e));
            Job::QuickConnectPolled(secret, r)
        });
    }
}

/// A feed scene's details (`full`: with its media sources, for its page).
/// Blocking: call from a job thread.
fn scene_details(
    src: &dyn fp_sources::Source,
    url: &str,
    full: bool,
) -> Result<fp_sources::SceneInfo, String> {
    let feed = src.as_scene_feed().ok_or(tr!("source-not-scene-feed"))?;
    feed.scene(url, full).map_err(|e| ui::errors::source(&e))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unindexed_owned_resource_keeps_its_source_pin() {
        let library = Library::open_in_memory().unwrap();
        let mut req = OpenRequest {
            location: fp_sources::location::encode("private", "http", "movie.mp4"),
            ..Default::default()
        };
        let pins = std::collections::BTreeMap::from([("private".into(), "a stored hash".into())]);
        let unlocked = std::collections::HashSet::new();
        let source = request_source(&library, &req).unwrap();
        assert_eq!(source, "private");
        assert!(crate::pin::locked(&pins, &unlocked, &source));
        req.source_id = Some("public".into());
        assert_eq!(request_source(&library, &req).as_deref(), Some("private"));
    }
}
