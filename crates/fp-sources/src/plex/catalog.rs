//! Maps Plex library and metadata responses to the shared catalog, keeping
//! Plex's item types, query keys and pagination inside this adapter.

use super::{
    BACKDROP, LANDSCAPE, LIBRARY_TILE, LISTING_LIMIT, PAGE_LIMIT, POSTER, PlexSource, container,
    safe_resource,
};
use crate::catalog::{
    Catalog, CatalogCapabilities, CatalogFacets, CatalogFilter, CatalogHome, CatalogItem,
    CatalogKind, CatalogPage, CatalogQuery, CatalogRow, CatalogSort, CatalogVersion, RowShape,
};
use crate::error::{Error, Result};
use crate::json::{array, boolean, num, text, uint};
use crate::urlutil::encode_form_value;
use fp_core::format::detect_from_name;
use fp_core::sync::lock;
use serde_json::Value;
use std::collections::HashSet;

/// The videos in progress and next up, directly: `/hubs/continueWatching`
/// without `/items` wraps them in a hub.
const CONTINUE_WATCHING: &str = "/hubs/continueWatching/items";

/// The section fields that can hold a library's tile picture, best first: a
/// picture set on the library, then the collage the server makes of its
/// contents, then its poster.
const TILE_FIELDS: [&str; 3] = ["art", "composite", "thumb"];

/// How many items each of the server's rows is asked for.
const ROW_ITEMS: usize = 12;

/// The hub types that are not video: the server's music and photo rows.
const NON_VIDEO_HUBS: [&str; 6] = ["artist", "album", "track", "photo", "station", "podcast"];

/// What tells a collection's id from a video's: its rating key names a
/// collection, which lists elsewhere on the server.
const COLLECTION: &str = "collection:";
/// What tells a playlist's id from a video's, for the same reason.
const PLAYLIST: &str = "playlist:";

/// The server's own key for an item id: a collection or playlist id without
/// the prefix that tells it from a video.
fn bare_id(id: &str) -> &str {
    id.strip_prefix(COLLECTION)
        .or_else(|| id.strip_prefix(PLAYLIST))
        .unwrap_or(id)
}

/// Whether a path a server named for a row's listing is one this app may ask
/// for: on the server, in the parts of it that list media, and never one of
/// the `/:/` actions.
fn hub_path_ok(path: &str) -> bool {
    safe_resource(path)
        && ["/hubs/", "/library/", "/playlists/"]
            .iter()
            .any(|prefix| path.starts_with(prefix))
        && !path.contains("/:/")
}

/// Whether a hub is the server's Continue Watching or On Deck, which the front
/// page already shows from its own request.
fn is_continue_watching(hub: &Value) -> bool {
    ["context", "hubIdentifier"].iter().any(|field| {
        text(hub, field).is_some_and(|s| {
            let s = s.to_ascii_lowercase();
            s.contains("continue") || s.contains("ondeck")
        })
    })
}

/// Whether a picture is one of the server's stock glyphs, which stand in for a
/// library that has no picture of its own.
fn stock(path: &str) -> bool {
    path.starts_with("/:/resources/")
}

impl PlexSource {
    /// The first of a library's `fields` that holds a picture of its own, not
    /// a stock glyph, scaled to fit `size`.
    fn section_picture(
        &self,
        section: &Value,
        fields: &[&str],
        size: (u32, u32),
    ) -> Option<String> {
        fields
            .iter()
            .filter_map(|field| text(section, field))
            .filter(|path| !stock(path))
            .find_map(|path| self.image(&path, size))
    }

    /// Whether the user left library `section` out of this source.
    fn hidden(&self, section: &str) -> bool {
        lock(&self.config)
            .hidden_sections
            .iter()
            .any(|hidden| hidden == section)
    }

    /// Whether a reply's item may be shown: it is not in a library left out.
    /// One that does not say which library it is in is shown.
    pub(super) fn shown(&self, v: &Value) -> bool {
        text(v, "librarySectionID").is_none_or(|section| !self.hidden(&section))
    }

    /// The items of `raw` that are videos, collections or playlists in
    /// libraries not left out.
    fn shown_items(&self, raw: &[Value]) -> Vec<CatalogItem> {
        raw.iter()
            .filter(|v| self.shown(v))
            .filter_map(|v| self.catalog_item(v))
            .collect()
    }

    /// `libraries` without those left out.
    fn without_hidden(&self, mut libraries: Vec<CatalogItem>) -> Vec<CatalogItem> {
        libraries.retain(|library| {
            library
                .id
                .strip_prefix("section:")
                .is_none_or(|section| !self.hidden(section))
        });
        libraries
    }

    /// The libraries not left out: what the front page and the source's own
    /// folder list show.
    pub(super) fn visible_libraries(&self) -> Result<Vec<CatalogItem>> {
        Ok(self.without_hidden(self.libraries()?))
    }

    /// Every library of the server, those left out too: what testing a
    /// connection and the source's settings go by.
    pub(super) fn libraries(&self) -> Result<Vec<CatalogItem>> {
        self.client()?;
        // Sections is available to shared users too; section/all is the
        // main provider's newer endpoint and is not universal on older PMS.
        let v = match self.json("/library/sections") {
            Ok(v) => v,
            Err(Error::NotFound(_)) => self.json("/library/sections/all")?,
            Err(e) => return Err(e),
        };
        Ok(array(container(&v), "Directory")
            .iter()
            .filter_map(|section| {
                let typ = text(section, "type")?;
                if !matches!(typ.as_str(), "movie" | "show" | "video") {
                    return None;
                }
                let id = text(section, "key")?;
                let item = CatalogItem {
                    id: format!("section:{id}"),
                    name: text(section, "title").unwrap_or_default(),
                    kind: CatalogKind::Library,
                    collection_type: Some(
                        match typ.as_str() {
                            "movie" => "movies",
                            "show" => "tvshows",
                            _ => "homevideos",
                        }
                        .into(),
                    ),
                    location: self.location("section", &id, None),
                    landscape: self.section_picture(section, &TILE_FIELDS, LIBRARY_TILE),
                    poster: self.section_picture(section, &["thumb"], POSTER),
                    backdrop: self.section_picture(section, &["art", "composite"], BACKDROP),
                    ..Default::default()
                };
                Some(item)
            })
            .collect())
    }

    pub(super) fn catalog_item(&self, v: &Value) -> Option<CatalogItem> {
        let key = text(v, "ratingKey")?;
        let kind = match text(v, "type")?.as_str() {
            "movie" if text(v, "subtype").as_deref() == Some("clip") => CatalogKind::Video,
            "movie" => CatalogKind::Movie,
            "show" => CatalogKind::Series,
            "season" => CatalogKind::Season,
            "episode" => CatalogKind::Episode,
            "clip" | "video" => CatalogKind::Video,
            "collection" => CatalogKind::BoxSet,
            // Music and photo playlists are not ours to show.
            "playlist" if text(v, "playlistType").is_none_or(|t| t == "video") => {
                CatalogKind::Playlist
            }
            "folder" => CatalogKind::Folder,
            _ => return None,
        };
        // A collection and a playlist list somewhere other than a video's
        // children, so their ids say what they are.
        let id = match kind {
            CatalogKind::BoxSet => format!("{COLLECTION}{key}"),
            CatalogKind::Playlist => format!("{PLAYLIST}{key}"),
            _ => key.clone(),
        };
        let versions: Vec<CatalogVersion> = array(v, "Media")
            .iter()
            .filter_map(|media| {
                let id = text(media, "id")?;
                let path = array(media, "Part").first().and_then(|p| text(p, "file"));
                let video = array(media, "Part")
                    .iter()
                    .flat_map(|p| array(p, "Stream"))
                    .find(|s| num(s, "streamType") == Some(1.0));
                let range = video.and_then(|s| {
                    if boolean(s, "DOVIPresent") == Some(true) {
                        Some("Dolby Vision".into())
                    } else if uint(s, "bitDepth").is_some_and(|n| n > 8)
                        && text(s, "colorTrc")
                            .is_some_and(|s| s.contains("smpte2084") || s.contains("arib"))
                    {
                        Some("HDR".into())
                    } else {
                        None
                    }
                });
                Some(CatalogVersion {
                    id,
                    name: text(media, "videoResolution")
                        .map(|s| {
                            if s.chars().all(|c| c.is_ascii_digit()) {
                                format!("{s}p")
                            } else {
                                s
                            }
                        })
                        .unwrap_or_default(),
                    height: uint(media, "height"),
                    codec: text(media, "videoCodec")
                        .or_else(|| video.and_then(|s| text(s, "codec"))),
                    range,
                    size: array(media, "Part").iter().try_fold(0u64, |sum, p| {
                        num(p, "size").map(|n| sum.saturating_add(n.max(0.0) as u64))
                    }),
                    format: path.as_deref().and_then(detect_from_name),
                })
            })
            .collect();
        let selected = lock(&self.versions).get(&key).cloned();
        let selected_media = self.choose_media(v, selected.as_deref()).ok();
        let format = selected_media
            .and_then(|m| array(m, "Part").first())
            .and_then(|p| text(p, "file"))
            .as_deref()
            .and_then(detect_from_name)
            .or_else(|| text(v, "title").as_deref().and_then(detect_from_name));
        // A playlist has no poster, only the collage the server makes of it.
        let thumb = text(v, "thumb")
            .or_else(|| text(v, "parentThumb"))
            .or_else(|| text(v, "grandparentThumb"))
            .or_else(|| text(v, "composite"));
        let art = text(v, "art").or_else(|| text(v, "grandparentArt"));
        // An episode's thumb is its still, wider than anything's poster.
        let wide = if kind == CatalogKind::Episode {
            thumb.as_ref().or(art.as_ref())
        } else {
            art.as_ref().or(thumb.as_ref())
        };
        let landscape = wide.and_then(|p| self.image(p, LANDSCAPE));
        let poster = thumb.and_then(|p| self.image(&p, POSTER));
        let backdrop = art.and_then(|p| self.image(&p, BACKDROP));
        let unplayed = num(v, "leafCount")
            .zip(num(v, "viewedLeafCount"))
            .map(|(n, v)| (n - v).max(0.0) as u32);
        // A show's or season's viewCount counts plays and stays up when
        // episodes are marked unwatched: only its episodes say it is finished.
        let viewed = match kind {
            CatalogKind::Series | CatalogKind::Season => {
                unplayed == Some(0) && num(v, "leafCount").is_some_and(|n| n > 0.0)
            }
            _ => num(v, "viewCount").unwrap_or(0.0) > 0.0,
        };
        Some(CatalogItem {
            id,
            name: text(v, "title").unwrap_or_default(),
            kind,
            overview: text(v, "summary"),
            year: num(v, "year").map(|n| n as i32),
            runtime: num(v, "duration").map(|n| n / 1000.0),
            community_rating: num(v, "audienceRating")
                .or_else(|| num(v, "rating"))
                .map(|n| n as f32),
            official_rating: text(v, "contentRating"),
            genres: tags(v, "Genre"),
            tags: Vec::new(),
            studios: text(v, "studio").into_iter().collect(),
            people: array(v, "Role")
                .iter()
                .filter_map(|p| Some((text(p, "tag")?, text(p, "role").unwrap_or_default())))
                .collect(),
            series_id: text(v, "grandparentRatingKey").or_else(|| {
                (kind == CatalogKind::Season)
                    .then(|| text(v, "parentRatingKey"))
                    .flatten()
            }),
            series_name: text(v, "grandparentTitle").or_else(|| {
                (kind == CatalogKind::Season)
                    .then(|| text(v, "parentTitle"))
                    .flatten()
            }),
            season_id: (kind == CatalogKind::Episode)
                .then(|| text(v, "parentRatingKey"))
                .flatten(),
            index: num(v, "index").map(|n| n.max(0.0) as u32),
            parent_index: num(v, "parentIndex").map(|n| n.max(0.0) as u32),
            child_count: num(v, "childCount")
                .or_else(|| num(v, "leafCount"))
                .map(|n| n.max(0.0) as u32),
            played: viewed,
            position: num(v, "viewOffset").map_or(0.0, |n| n / 1000.0),
            unplayed,
            added: num(v, "addedAt").map(|n| n as i64),
            landscape,
            poster,
            backdrop,
            location: self.location("item", &key, None),
            format,
            versions,
            version: selected,
            ..Default::default()
        })
    }

    pub(super) fn choose_media<'a>(
        &self,
        item: &'a Value,
        selected: Option<&str>,
    ) -> Result<&'a Value> {
        let media = array(item, "Media");
        if let Some(id) = selected {
            if let Some(chosen) = media.iter().find(|m| text(m, "id").as_deref() == Some(id)) {
                return Ok(chosen);
            }
            // Its file was replaced on the server. The video is still there.
            log::info!("Plex version {id} is gone; choosing another");
        }
        let max = lock(&self.config).max_height;
        media
            .iter()
            .filter(|m| max.is_none_or(|max| uint(m, "height").is_none_or(|h| h <= max)))
            .max_by_key(|m| uint(m, "height").unwrap_or(0))
            .or_else(|| {
                media
                    .iter()
                    .min_by_key(|m| uint(m, "height").unwrap_or(u32::MAX))
            })
            .ok_or_else(|| Error::Unsupported("Plex item has no playable media version".into()))
    }

    /// One request's page, and how many items the server sent: more than
    /// the page has when some aren't kept.
    fn page(&self, path: &str, query: &CatalogQuery) -> Result<(CatalogPage, usize)> {
        let v = self.json(path)?;
        let c = container(&v);
        let raw = array(c, "Metadata");
        // A row's or a collection's listing can hold items of any library.
        let mixed = query.hub.is_some()
            || query
                .parent
                .as_deref()
                .is_some_and(|p| p.starts_with(COLLECTION));
        let mut items: Vec<_> = raw
            .iter()
            .filter(|v| !mixed || self.shown(v))
            .filter_map(|v| self.catalog_item(v))
            .collect();
        if !query.types.is_empty() {
            items.retain(|i| {
                query.types.contains(&i.kind)
                    || (matches!(i.kind, CatalogKind::Movie | CatalogKind::Video)
                        && query
                            .types
                            .iter()
                            .any(|k| matches!(k, CatalogKind::Movie | CatalogKind::Video)))
            });
        }
        let offset = num(c, "offset").map_or(query.start, |n| n.max(0.0) as usize);
        if offset != query.start && !raw.is_empty() {
            return Err(Error::parse(
                "Plex page",
                "server returned a different page offset",
            ));
        }
        let total = num(c, "totalSize")
            .map(|n| n.max(0.0) as usize)
            .unwrap_or_else(|| offset.saturating_add(raw.len()));
        Ok((CatalogPage { items, total }, raw.len()))
    }

    /// Up to `query.limit` items from `query.start`, over as many requests as
    /// Plex's page limit takes. A grid reloading all it shows can ask for more
    /// than one page holds.
    fn listing(&self, query: &CatalogQuery) -> Result<CatalogPage> {
        let wanted = query.limit.max(1);
        let mut q = query.clone();
        let mut sent = 0;
        let mut items = Vec::new();
        let mut last_first = None;
        loop {
            q.limit = (wanted - sent).min(PAGE_LIMIT);
            let (page, raw) = self.page(&self.query_path(&q)?, &q)?;
            if wanted >= LISTING_LIMIT && page.total > LISTING_LIMIT {
                return Err(Error::Unsupported("Plex listing is too large".into()));
            }
            let first = page.items.first().map(|i| i.id.clone());
            if first.is_some() && first == last_first {
                return Err(Error::parse(
                    "Plex listing",
                    "server listing did not advance",
                ));
            }
            last_first = first;
            items.extend(page.items);
            sent += raw;
            q.start += raw;
            if raw == 0 || sent >= wanted || q.start >= page.total {
                return Ok(CatalogPage {
                    items,
                    total: page.total,
                });
            }
        }
    }

    pub(super) fn whole_listing(&self, query: CatalogQuery) -> Result<Vec<CatalogItem>> {
        let page = self.items(&CatalogQuery {
            start: 0,
            limit: LISTING_LIMIT,
            ..query
        })?;
        Ok(page.items)
    }

    fn query_path(&self, query: &CatalogQuery) -> Result<String> {
        if query.filter == CatalogFilter::Favorites
            || query.min_width.is_some()
            || query.person.is_some()
            || !query.tags.is_empty()
        {
            return Err(Error::Unsupported(
                "this Plex catalog filter is not available".into(),
            ));
        }
        let parent = query.parent.as_deref();
        let (path, section, inherited) = if let Some(hub) = query.hub.as_deref() {
            let (path, kept) = hub_listing(hub)?;
            (path, None, kept)
        } else if let Some(section) = parent.and_then(|p| p.strip_prefix("section:")) {
            (
                format!("/library/sections/{}/all", encode_form_value(section)),
                Some(section),
                Vec::new(),
            )
        } else if let Some(key) = parent.and_then(|p| p.strip_prefix(COLLECTION)) {
            (
                format!("/library/collections/{}/children", encode_form_value(key)),
                None,
                Vec::new(),
            )
        } else if let Some(key) = parent.and_then(|p| p.strip_prefix(PLAYLIST)) {
            (
                format!("/playlists/{}/items", encode_form_value(key)),
                None,
                Vec::new(),
            )
        } else if let Some(p) = parent {
            (
                format!("/library/metadata/{}/children", encode_form_value(p)),
                None,
                Vec::new(),
            )
        } else {
            ("/library/all".to_string(), None, Vec::new())
        };
        let mut url =
            url::Url::parse(&format!("http://placeholder{path}")).expect("an encoded path");
        {
            let mut p = url.query_pairs_mut();
            for (key, value) in &inherited {
                p.append_pair(key, value);
            }
            p.append_pair("X-Plex-Container-Start", &query.start.to_string());
            p.append_pair(
                "X-Plex-Container-Size",
                &query.limit.clamp(1, PAGE_LIMIT).to_string(),
            );
            // The server's own order sends no sort at all.
            let key = match query.sort {
                CatalogSort::Name => Some("titleSort"),
                CatalogSort::Added => Some("addedAt"),
                CatalogSort::Released => Some("originallyAvailableAt"),
                CatalogSort::Rating => Some("audienceRating"),
                CatalogSort::Runtime => Some("duration"),
                CatalogSort::LastWatched => Some("lastViewedAt"),
                CatalogSort::Random => Some("random"),
                CatalogSort::Server => None,
            };
            if let Some(key) = key {
                p.append_pair(
                    "sort",
                    &format!("{key}:{}", if query.descending { "desc" } else { "asc" }),
                );
            }
            // A row's listing already says what it holds.
            if query.hub.is_none()
                && let Some(kind) = query.types.first().and_then(|k| type_number(*k))
            {
                p.append_pair("type", kind);
            }
            if !query.search.trim().is_empty() {
                p.append_pair("title", query.search.trim());
            }
            match query.filter {
                CatalogFilter::Unplayed => {
                    p.append_pair("unwatched", "1");
                }
                CatalogFilter::Resumable => {
                    p.append_pair("inProgress", "1");
                }
                _ => {}
            }
            for (key, values) in [("genre", &query.genres), ("studio", &query.studios)] {
                if !values.is_empty() {
                    let section = section.ok_or_else(|| {
                        Error::Unsupported("Plex facets require a library".into())
                    })?;
                    let values: Vec<_> = values
                        .iter()
                        .map(|name| {
                            if key == "studio" {
                                // Studios are strings, not genre IDs. PMS's
                                // choice key may already be URL-encoded. It
                                // splits values at commas, then decodes each,
                                // so a comma in a name is escaped once more.
                                name.replace('%', "%25").replace(',', "%2C")
                            } else {
                                lock(&self.facets)
                                    .get(&format!("{section}/{key}/{name}"))
                                    .cloned()
                                    .unwrap_or_else(|| name.clone())
                            }
                        })
                        .collect();
                    // String equality is `studio==...`; plain `studio=...`
                    // performs a substring match instead.
                    p.append_pair(
                        if key == "studio" { "studio=" } else { key },
                        &values.join(","),
                    );
                }
            }
            if !query.years.is_empty() {
                p.append_pair(
                    "year",
                    &query
                        .years
                        .iter()
                        .map(ToString::to_string)
                        .collect::<Vec<_>>()
                        .join(","),
                );
            }
        }
        Ok(format!(
            "{}?{}",
            url.path(),
            url.query().unwrap_or_default()
        ))
    }

    fn facet(&self, section: &str, kind: &str) -> Result<Vec<String>> {
        let v = self.json(&format!(
            "/library/sections/{}/{kind}",
            encode_form_value(section)
        ))?;
        let mut names = Vec::new();
        for d in array(container(&v), "Directory") {
            if let (Some(name), Some(key)) = (text(d, "title"), text(d, "key")) {
                lock(&self.facets).insert(format!("{section}/{kind}/{name}"), key);
                names.push(name);
            }
        }
        Ok(names)
    }
}

impl Catalog for PlexSource {
    fn capabilities(&self) -> CatalogCapabilities {
        CatalogCapabilities {
            favorites: false,
            width_filter: false,
            people: false,
            tags: false,
            library_pages: true,
        }
    }

    fn all_libraries(&self) -> Result<Vec<CatalogItem>> {
        self.libraries()
    }

    fn home(&self) -> Result<CatalogHome> {
        let all = self.libraries()?;
        // With every library left out the page is empty: the server's rows
        // for all of them are not asked for.
        let any = !all.is_empty();
        let libraries = self.without_hidden(all);
        let nothing_shown = any && libraries.is_empty();
        let sections: Vec<String> = libraries
            .iter()
            .filter_map(|library| library.id.strip_prefix("section:"))
            .map(str::to_string)
            .collect();
        let resume = match self.json(&format!("{CONTINUE_WATCHING}?X-Plex-Container-Size=20")) {
            Ok(v) => self.shown_items(array(container(&v), "Metadata")),
            Err(e) => {
                log::info!("continue watching on {}: {e}", self.name);
                Vec::new()
            }
        };
        let rows = if nothing_shown {
            Vec::new()
        } else {
            self.server_rows(&sections).unwrap_or_else(|e| {
                log::info!("home rows on {}: {e}", self.name);
                Vec::new()
            })
        };
        // A server that gives no rows of its own still gets the newest of each
        // library.
        let mut latest = Vec::new();
        if rows.is_empty() {
            for library in &libraries {
                let page = self.items(&CatalogQuery {
                    parent: Some(library.id.clone()),
                    sort: CatalogSort::Added,
                    descending: true,
                    limit: ROW_ITEMS,
                    ..Default::default()
                });
                match page {
                    Ok(page) if !page.items.is_empty() => {
                        latest.push((library.clone(), page.items));
                    }
                    Ok(_) => {}
                    Err(e) => log::info!("latest in {} on {}: {e}", library.name, self.name),
                }
            }
        }
        Ok(CatalogHome {
            libraries,
            resume,
            next_up: Vec::new(),
            latest,
            rows,
        })
    }

    fn library_rows(&self, library: &str) -> Result<Vec<CatalogRow>> {
        let Some(section) = library.strip_prefix("section:") else {
            return Ok(Vec::new());
        };
        let path = format!(
            "/hubs/sections/{}?count={ROW_ITEMS}",
            encode_form_value(section)
        );
        match self.json(&path) {
            Ok(v) => Ok(self.hub_rows(&v, false)),
            // A server too old for library hubs opens its libraries as grids.
            Err(Error::NotFound(_)) => Ok(Vec::new()),
            Err(e) => Err(e),
        }
    }

    fn search(&self, query: &str, limit: usize) -> Result<Vec<(&'static str, Vec<CatalogItem>)>> {
        let v = self.json(&format!(
            "/hubs/search?query={}&limit={}",
            encode_form_value(query),
            limit.clamp(1, PAGE_LIMIT)
        ))?;
        let mut groups: Vec<(&'static str, Vec<CatalogItem>)> = Vec::new();
        for hub in array(container(&v), "Hub") {
            let typ = text(hub, "type").unwrap_or_default();
            let title = match typ.as_str() {
                "movie" => "Movies",
                "show" => "Shows",
                "collection" => "Collections",
                "episode" | "clip" => "Videos",
                _ => continue,
            };
            let found = self.shown_items(array(hub, "Metadata"));
            if found.is_empty() {
                continue;
            }
            if let Some((_, items)) = groups.iter_mut().find(|(k, _)| *k == title) {
                items.extend(found);
            } else {
                groups.push((title, found));
            }
        }
        Ok(groups)
    }

    fn items(&self, query: &CatalogQuery) -> Result<CatalogPage> {
        if !query.ids.is_empty() {
            let mut items = Vec::new();
            for chunk in query.ids.chunks(60) {
                let ids = chunk
                    .iter()
                    .map(|id| encode_form_value(bare_id(id)))
                    .collect::<Vec<_>>()
                    .join(",");
                // A playlist is not in the library's metadata; ask for each.
                let found = if chunk.iter().any(|id| id.starts_with(PLAYLIST)) {
                    Err(Error::NotFound("Plex playlist".into()))
                } else {
                    self.json(&format!("/library/metadata/{ids}"))
                };
                match found {
                    Ok(value) => {
                        let mut found: std::collections::HashMap<_, _> =
                            array(container(&value), "Metadata")
                                .iter()
                                .filter_map(|v| self.catalog_item(v).map(|i| (i.id.clone(), i)))
                                .collect();
                        items.extend(chunk.iter().filter_map(|id| found.remove(id)));
                    }
                    Err(Error::NotFound(_)) => {
                        for id in chunk {
                            match self.item(id) {
                                Ok(item) => items.push(item),
                                Err(Error::NotFound(_)) => {}
                                Err(e) => return Err(e),
                            }
                        }
                    }
                    Err(e) => return Err(e),
                }
            }
            return Ok(CatalogPage {
                total: items.len(),
                items,
            });
        }
        self.listing(query)
    }

    fn shuffled(&self, query: &CatalogQuery) -> Result<Vec<String>> {
        let listed = self.whole_listing(CatalogQuery {
            sort: CatalogSort::Name,
            ..query.clone()
        })?;
        let mut seen = HashSet::new();
        let mut ids: Vec<String> = listed
            .into_iter()
            .map(|item| item.id)
            .filter(|id| seen.insert(id.clone()))
            .collect();
        // PMS random pagination is not stable. Snapshot by a stable sort,
        // then assign each id a local seeded key exactly once.
        let seed = fp_core::playback::now_ms() as u64;
        ids.sort_by_cached_key(|id| fp_core::shuffle::key(seed, id));
        Ok(ids)
    }

    fn facets(&self, query: &CatalogQuery) -> Result<CatalogFacets> {
        let Some(section) = query
            .parent
            .as_deref()
            .and_then(|p| p.strip_prefix("section:"))
        else {
            return Ok(CatalogFacets::default());
        };
        let genres = self.facet(section, "genre")?;
        let studios = self.facet(section, "studio")?;
        let mut years: Vec<i32> = self
            .facet(section, "year")?
            .iter()
            .filter_map(|s| s.parse().ok())
            .collect();
        years.sort_unstable_by(|a, b| b.cmp(a));
        Ok(CatalogFacets {
            genres,
            studios,
            years,
            tags: Vec::new(),
        })
    }

    fn item(&self, id: &str) -> Result<CatalogItem> {
        if id.starts_with("section:") {
            return self
                .libraries()?
                .into_iter()
                .find(|i| i.id == id)
                .ok_or_else(|| Error::NotFound("Plex library".into()));
        }
        if let Some(key) = id.strip_prefix(PLAYLIST) {
            let v = self.json(&format!("/playlists/{}", encode_form_value(key)))?;
            return array(container(&v), "Metadata")
                .first()
                .and_then(|playlist| self.catalog_item(playlist))
                .ok_or_else(|| Error::NotFound("Plex playlist".into()));
        }
        self.catalog_item(&self.raw_item(bare_id(id))?)
            .ok_or_else(|| Error::Unsupported("Plex item is not a video".into()))
    }

    fn seasons(&self, series: &str) -> Result<Vec<CatalogItem>> {
        self.all_children(series, Some(CatalogKind::Season))
    }
    fn episodes(&self, series: &str, season: Option<&str>) -> Result<Vec<CatalogItem>> {
        match season {
            Some(season) => self.all_children(season, Some(CatalogKind::Episode)),
            None => {
                let mut episodes = Vec::new();
                for season in self.seasons(series)? {
                    episodes.extend(self.all_children(&season.id, Some(CatalogKind::Episode))?);
                }
                Ok(episodes)
            }
        }
    }
    fn next_up(&self, series: Option<&str>, limit: usize) -> Result<Vec<CatalogItem>> {
        let v = self.json(&format!("{CONTINUE_WATCHING}?X-Plex-Container-Size=100"))?;
        Ok(self
            .shown_items(array(container(&v), "Metadata"))
            .into_iter()
            .filter(|i| {
                i.kind == CatalogKind::Episode
                    && series.is_none_or(|s| i.series_id.as_deref() == Some(s))
            })
            .take(limit)
            .collect())
    }
    fn set_played(&self, id: &str, played: bool) -> Result<()> {
        self.request(
            "PUT",
            &format!(
                "/:/{}?identifier=com.plexapp.plugins.library&key={}",
                if played { "scrobble" } else { "unscrobble" },
                encode_form_value(bare_id(id))
            ),
            &[],
        )?;
        // Marking a show marks its episodes too.
        lock(&self.metadata).clear();
        Ok(())
    }
    fn set_version(&self, location: &str, version: Option<&str>) {
        let Ok(location) = self.parse_location(location) else {
            return;
        };
        if location.kind != "item" {
            return;
        }
        let id = location.id;
        let mut versions = lock(&self.versions);
        match version {
            Some(v) => {
                versions.insert(id.to_string(), v.to_string());
            }
            None => {
                versions.remove(&id);
            }
        }
    }
}

impl PlexSource {
    /// The rows of the server's front page, in its order: what Plex Web shows
    /// for these libraries. Empty when the server names none.
    fn server_rows(&self, sections: &[String]) -> Result<Vec<CatalogRow>> {
        let mut promoted = url::form_urlencoded::Serializer::new(String::new());
        promoted
            .append_pair("count", &ROW_ITEMS.to_string())
            .append_pair("excludeContinueWatching", "1");
        let mut global = url::form_urlencoded::Serializer::new(String::new());
        global.append_pair("count", &ROW_ITEMS.to_string());
        if !sections.is_empty() {
            let sections = sections.join(",");
            promoted
                .append_pair("contentDirectoryID", &sections)
                .append_pair("pinnedContentDirectoryID", &sections);
            global.append_pair("contentDirectoryID", &sections);
        }
        for path in [
            format!("/hubs/promoted?{}", promoted.finish()),
            format!("/hubs?{}", global.finish()),
        ] {
            match self.json(&path) {
                Ok(v) => {
                    let rows = self.hub_rows(&v, true);
                    if !rows.is_empty() {
                        return Ok(rows);
                    }
                }
                Err(e @ Error::SignedOut(_)) => return Err(e),
                Err(e) => log::info!("hubs on {}: {e}", self.name),
            }
        }
        Ok(Vec::new())
    }

    /// The rows a server's hubs make: its titles in its order, one card
    /// shape each. A row keeps the items that are videos, collections or
    /// playlists, and is left out when none are. With `home`, the hubs that
    /// are Continue Watching are left out, as the page has them already.
    fn hub_rows(&self, v: &Value, home: bool) -> Vec<CatalogRow> {
        let mut rows: Vec<CatalogRow> = Vec::new();
        for hub in array(container(v), "Hub") {
            let kind = text(hub, "type").unwrap_or_default();
            if NON_VIDEO_HUBS.contains(&kind.as_str()) || (home && is_continue_watching(hub)) {
                continue;
            }
            let hub_section = text(hub, "librarySectionID");
            if hub_section.as_deref().is_some_and(|s| self.hidden(s)) {
                continue;
            }
            let raw: Vec<&Value> = array(hub, "Metadata")
                .iter()
                .filter(|v| self.shown(v))
                .collect();
            let items: Vec<_> = raw.iter().filter_map(|v| self.catalog_item(v)).collect();
            let title = text(hub, "title").unwrap_or_default();
            if items.len() < raw.len() {
                log::debug!(
                    "{} of {} items in \"{title}\" on {} are not videos",
                    raw.len() - items.len(),
                    raw.len(),
                    self.name
                );
            }
            if items.is_empty() {
                continue;
            }
            let key = text(hub, "key");
            let mut id = text(hub, "hubIdentifier")
                .or_else(|| key.clone())
                .unwrap_or_else(|| title.clone());
            if rows.iter().any(|row| row.id == id) {
                id = format!("{id}#{}", rows.len());
            }
            let more = key
                .filter(|key| boolean(hub, "more") == Some(true) && hub_listing(key).is_ok())
                .map(|key| CatalogQuery {
                    hub: Some(key),
                    sort: CatalogSort::Server,
                    ..Default::default()
                });
            let library = hub_section
                .or_else(|| raw.iter().find_map(|m| text(m, "librarySectionID")))
                .map(|section| format!("section:{section}"));
            rows.push(CatalogRow {
                id,
                title,
                shape: RowShape::of(&items),
                items,
                more,
                library,
            });
        }
        rows
    }

    fn all_children(&self, id: &str, kind: Option<CatalogKind>) -> Result<Vec<CatalogItem>> {
        let mut items = self.whole_listing(CatalogQuery {
            parent: Some(id.into()),
            types: kind.into_iter().collect(),
            ..Default::default()
        })?;
        if matches!(kind, Some(CatalogKind::Season | CatalogKind::Episode)) {
            items.sort_by_key(|i| (i.parent_index, i.index));
        }
        Ok(items)
    }
}

/// The path and query of a row's listing as the server named it, less its
/// paging, if it is a listing this app may ask for.
fn hub_listing(hub: &str) -> Result<(String, Vec<(String, String)>)> {
    let refused = || Error::invalid(hub, "Plex row listing must stay on its server");
    if !hub_path_ok(hub) {
        return Err(refused());
    }
    let url = url::Url::parse(&format!("http://placeholder{hub}")).map_err(|_| refused())?;
    // Parsing resolves dot segments, even encoded ones, so check what is left.
    if url.host_str() != Some("placeholder") || !hub_path_ok(url.path()) {
        return Err(refused());
    }
    let kept = url
        .query_pairs()
        .filter(|(key, _)| !key.starts_with("X-Plex-Container-"))
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect();
    Ok((url.path().to_string(), kept))
}

fn tags(v: &Value, key: &str) -> Vec<String> {
    array(v, key)
        .iter()
        .filter_map(|t| text(t, "tag"))
        .collect()
}
fn type_number(kind: CatalogKind) -> Option<&'static str> {
    match kind {
        CatalogKind::Movie | CatalogKind::Video | CatalogKind::MusicVideo => Some("1"),
        CatalogKind::Series => Some("2"),
        CatalogKind::Season => Some("3"),
        CatalogKind::Episode => Some("4"),
        CatalogKind::BoxSet => Some("18"),
        _ => None,
    }
}
