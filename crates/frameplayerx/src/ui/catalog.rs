//! A media-server catalog: the server's libraries and rows of what to watch
//! next, poster grids the server searches, sorts, shuffles and filters
//! (genres, decades, resolution, studios, tags), series with their seasons
//! and episodes, and details pages. Watched marks, resume points and
//! favourites are the server's own.

use super::page::{self, Card};
use super::theme::{self, Weight};
use super::widgets::{self, Kind, Tip};
use super::{Action, View, fmt_time, icons};
use crate::playback::OpenRequest;
use egui::{Align2, Color32, RichText, Sense, Vec2};
use fp_sources::{
    Catalog, CatalogCapabilities, CatalogFacets, CatalogFilter, CatalogHome, CatalogItem,
    CatalogKind, CatalogPage, CatalogQuery, CatalogRow, CatalogSort, RowShape,
};
use std::collections::HashMap;
use std::time::{Duration, Instant};

/// Typing rests this long before a search goes to the server.
const SEARCH_DELAY: Duration = Duration::from_millis(600);
/// Items per page of a grid.
const PAGE: usize = 60;
/// The widths the Resolution filter keeps at least.
const RESOLUTIONS: [(u32, &str); 4] = [(1280, "HD"), (3840, "4K"), (5760, "6K"), (7680, "8K")];
/// Matches shown per section of a search.
const SEARCH_LIMIT: usize = 50;
/// Height of the banners of series and details pages.
const BANNER: f32 = 320.0;
/// A page that loaded this recently is not asked for again when the
/// browser is shown.
const FRESH: Duration = Duration::from_secs(30);
/// How long a server's front page stays good for showing at once when the
/// server is opened again.
const HOME_LIFE: Duration = Duration::from_secs(6 * 3600);

/// The pages open on a server, the one shown last.
pub struct CatalogView {
    pub pages: Vec<Page>,
}

pub struct Page {
    /// The page's identity; its facets use it independently of data loads.
    pub seq: u64,
    /// Only the latest listing/search/details request can update the page.
    pub(super) request_seq: Option<u64>,
    pub loading: bool,
    pub error: Option<String>,
    /// When the server last answered this page.
    pub loaded_at: Option<Instant>,
    pub body: Body,
}

pub enum Body {
    /// Libraries and rows of what to watch.
    Home {
        home: Option<CatalogHome>,
        search: String,
        /// When the search text last changed.
        typed: Option<Instant>,
    },
    /// What a search of the server found, a row per kind (films, series,
    /// collections, other videos), as Jellyfin shows it.
    Search {
        /// The search field, in the header.
        search: String,
        typed: Option<Instant>,
        /// What the results are for.
        asked: String,
        results: Vec<(&'static str, Vec<CatalogItem>)>,
    },
    /// A library, collection or folder, a page at a time.
    Grid(Box<Grid>),
    /// A library of a server that has pages for them: tabs for its own
    /// rows, its titles and its collections.
    Library(Box<LibraryPage>),
    Series {
        series: Box<CatalogItem>,
        seasons: Vec<CatalogItem>,
        season: Option<String>,
        episodes: Vec<CatalogItem>,
        next_up: Option<Box<CatalogItem>>,
    },
    /// A film, episode or video.
    Item { item: Box<CatalogItem> },
}

impl Body {
    /// The listing the page shows, if it shows one.
    fn grid_mut(&mut self) -> Option<&mut Grid> {
        match self {
            Body::Grid(g) => Some(g.as_mut()),
            Body::Library(l) => {
                let tab = l.tab;
                l.grid_of(tab)
            }
            _ => None,
        }
    }
}

/// A library, collection or folder, a page at a time.
pub struct Grid {
    pub title: String,
    pub query: CatalogQuery,
    pub items: Vec<CatalogItem>,
    pub total: usize,
    pub typed: Option<Instant>,
    /// Every item of a shuffle, in its order, which its pages follow.
    pub order: Option<Vec<String>>,
    pub facets: Facets,
    pub filters_open: bool,
    /// How the drawer's long sections are narrowed, by title.
    pub lists: HashMap<&'static str, page::ListState>,
    /// The tags the page is for ("Tagged ..."), which are not filters.
    pub base_tags: Vec<String>,
    /// Sorted or filtered anew: the grid goes back to its top.
    pub to_top: bool,
    /// The listing began in the server's own order (a collection, a
    /// playlist or a row's "See all"), which stays one of its sorts.
    pub server_order: bool,
}

/// The tabs of a library's page.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum LibraryTab {
    /// The library's own rows (Plex's Recommended).
    #[default]
    Recommended,
    /// Its titles as a grid.
    Library,
    Collections,
}

impl LibraryTab {
    const ALL: [LibraryTab; 3] = [
        LibraryTab::Recommended,
        LibraryTab::Library,
        LibraryTab::Collections,
    ];

    /// Where the tab keeps its flags in a [`LibraryPage`].
    fn index(self) -> usize {
        self as usize
    }
}

/// A library of a server that has pages for them. A tab asks the server
/// when it is first shown, never before.
pub struct LibraryPage {
    pub library: CatalogItem,
    pub tab: LibraryTab,
    /// The library's own rows; `None` until they come.
    pub rows: Option<Vec<CatalogRow>>,
    pub grid: Grid,
    pub collections: Grid,
    /// Per tab: whether the server was asked for it, and whether it
    /// answered.
    asked: [bool; 3],
    got: [bool; 3],
    /// The tab the request in flight is for.
    target: LibraryTab,
}

impl LibraryPage {
    /// A library page that has asked for its rows and for nothing else.
    fn new(library: CatalogItem) -> LibraryPage {
        let titles = CatalogQuery {
            limit: PAGE,
            ..CatalogQuery::children(&library)
        };
        let collections = CatalogQuery {
            parent: Some(library.id.clone()),
            types: vec![CatalogKind::BoxSet],
            limit: PAGE,
            ..CatalogQuery::default()
        };
        LibraryPage {
            grid: Grid::new(library.name.clone(), titles),
            collections: Grid::new(library.name.clone(), collections),
            library,
            tab: LibraryTab::Recommended,
            rows: None,
            asked: [true, false, false],
            got: [false; 3],
            target: LibraryTab::Recommended,
        }
    }

    /// The tabs shown. A server with no rows for the library has no
    /// Recommended.
    fn tabs(&self) -> Vec<(LibraryTab, &'static str)> {
        let mut tabs = Vec::new();
        if self.rows.as_ref().is_none_or(|rows| !rows.is_empty()) {
            tabs.push((LibraryTab::Recommended, tr!("catalog-recommended")));
        }
        tabs.push((LibraryTab::Library, tr!("catalog-library")));
        tabs.push((LibraryTab::Collections, tr!("catalog-collections")));
        tabs
    }

    /// The listing of a tab, if it is one.
    fn grid_of(&mut self, tab: LibraryTab) -> Option<&mut Grid> {
        match tab {
            LibraryTab::Recommended => None,
            LibraryTab::Library => Some(&mut self.grid),
            LibraryTab::Collections => Some(&mut self.collections),
        }
    }

    /// What brings a tab up to date.
    fn load_for(&self, tab: LibraryTab) -> Load {
        match tab {
            LibraryTab::Recommended => Load::LibraryRows(self.library.id.clone()),
            LibraryTab::Library => self.grid.reload_load(),
            LibraryTab::Collections => self.collections.reload_load(),
        }
    }

    /// What to ask for now: the tab shown, when it was not asked for yet
    /// and nothing else is loading.
    fn next_load(&self, loading: bool) -> Option<Load> {
        if loading || self.asked[self.tab.index()] {
            return None;
        }
        Some(self.load_for(self.tab))
    }

    /// Shows a tab the user chose. One that failed to load is asked for
    /// again.
    fn show(&mut self, tab: LibraryTab) {
        self.tab = tab;
        if !self.got[tab.index()] {
            self.asked[tab.index()] = false;
        }
        if let Some(g) = self.grid_of(tab) {
            g.to_top = true;
        }
    }

    /// Every item the page holds.
    fn items_mut(&mut self) -> impl Iterator<Item = &mut CatalogItem> {
        self.rows
            .iter_mut()
            .flatten()
            .flat_map(|r| r.items.iter_mut())
            .chain(self.grid.items.iter_mut())
            .chain(self.collections.items.iter_mut())
    }
}

/// A grid's genres, years, studios and tags, asked for when its Filters
/// drawer first opens.
pub enum Facets {
    Unasked,
    Asked,
    Got(CatalogFacets),
    Failed(String),
}

/// One filter that is on, as its chip under the sorts names it.
enum On {
    Genre(String),
    Decade(i32),
    Width(u32),
    Studio(String),
    Tag(String),
}

impl Grid {
    fn new(title: String, query: CatalogQuery) -> Grid {
        Grid {
            title,
            base_tags: query.tags.clone(),
            server_order: query.sort == CatalogSort::Server,
            query,
            items: Vec::new(),
            total: 0,
            typed: None,
            order: None,
            facets: Facets::Unasked,
            filters_open: false,
            lists: Default::default(),
            to_top: false,
        }
    }

    fn on(&self) -> Vec<On> {
        let q = &self.query;
        let mut on: Vec<On> = q.genres.iter().cloned().map(On::Genre).collect();
        on.extend(decades_on(&q.years).into_iter().map(On::Decade));
        on.extend(q.min_width.map(On::Width));
        on.extend(q.studios.iter().cloned().map(On::Studio));
        on.extend(
            q.tags
                .iter()
                .filter(|t| !self.base_tags.contains(t))
                .cloned()
                .map(On::Tag),
        );
        on
    }

    /// The chips of the filters that are on.
    fn labels(&self) -> Vec<String> {
        self.on()
            .into_iter()
            .map(|o| match o {
                On::Genre(g) => tr!("filter-on-genre", genre = g),
                On::Decade(d) => tr!("filter-decade", decade = d),
                On::Width(w) => tr!("filter-width-and-up", width = width_label(w)),
                On::Studio(s) => tr!("filter-on-studio", studio = s),
                On::Tag(t) => tr!("filter-on-tag", tag = t),
            })
            .collect()
    }

    /// Takes off the filter of chip `i` (see [`Grid::labels`]).
    fn take_off(&mut self, i: usize) {
        let on = self.on().into_iter().nth(i);
        let q = &mut self.query;
        match on {
            Some(On::Genre(g)) => q.genres.retain(|x| *x != g),
            Some(On::Decade(d)) => q.years.retain(|y| !(d..d + 10).contains(y)),
            Some(On::Width(_)) => q.min_width = None,
            Some(On::Studio(s)) => q.studios.retain(|x| *x != s),
            Some(On::Tag(t)) => q.tags.retain(|x| *x != t),
            None => {}
        }
    }

    fn clear_filters(&mut self) {
        let q = &mut self.query;
        q.genres.clear();
        q.years.clear();
        q.min_width = None;
        q.studios.clear();
        q.tags.retain(|t| self.base_tags.contains(t));
    }

    /// A changed filter/search/sort starts a new listing, including a new
    /// shuffle snapshot; old page IDs must not leak into a later refresh.
    fn restart(&mut self) -> CatalogQuery {
        self.query.start = 0;
        self.query.limit = PAGE;
        self.query.ids.clear();
        self.order = None;
        self.to_top = true;
        self.query.clone()
    }

    /// What shows this listing again as it is now: the same shuffle, or
    /// the pages shown so far.
    fn reload_load(&self) -> Load {
        let shown = self.items.len().max(PAGE);
        Load::Grid(match &self.order {
            Some(o) => CatalogQuery {
                start: 0,
                ids: o.iter().take(shown).cloned().collect(),
                ..self.query.clone()
            },
            None => CatalogQuery {
                start: 0,
                limit: shown,
                ..self.query.clone()
            },
        })
    }

    /// Takes a page of answers in: a first page replaces the items, a
    /// later one adds to them.
    fn took(&mut self, page: CatalogPage, append: bool, order: Option<Vec<String>>, by_id: bool) {
        if append {
            self.items.extend(page.items);
        } else {
            self.items = page.items;
        }
        if order.is_some() {
            self.order = order;
        } else if !by_id && !append {
            self.order = None;
        }
        self.total = match &self.order {
            Some(o) => o.len(),
            None => page.total.max(self.items.len()),
        };
    }

    /// Choosing another action this frame must leave an unsent facets
    /// request eligible for the next frame.
    fn unasked_facets(&self) -> Option<CatalogQuery> {
        matches!(self.facets, Facets::Unasked).then(|| self.query.clone())
    }

    /// The next page: of a shuffle, its next items by id.
    fn next_page(&self) -> CatalogQuery {
        let start = self.items.len();
        let ids = self
            .order
            .as_ref()
            .map(|o| o.iter().skip(start).take(PAGE).cloned().collect())
            .unwrap_or_default();
        CatalogQuery {
            start,
            limit: PAGE,
            ids,
            ..self.query.clone()
        }
    }
}

/// The decades `years` make, newest first.
fn decades(years: &[i32]) -> Vec<i32> {
    let mut d: Vec<i32> = years.iter().map(|y| y - y.rem_euclid(10)).collect();
    d.sort_unstable_by(|a, b| b.cmp(a));
    d.dedup();
    d
}

/// The decades a query keeps whole (the Released filter adds and takes
/// off a decade's ten years).
fn decades_on(years: &[i32]) -> Vec<i32> {
    decades(years)
        .into_iter()
        .filter(|d| (*d..*d + 10).all(|y| years.contains(&y)))
        .collect()
}

fn width_label(w: u32) -> &'static str {
    RESOLUTIONS
        .iter()
        .find(|(x, _)| *x == w)
        .map_or("", |(_, label)| label)
}

/// The front pages of the servers last seen, so a server opens at once
/// and the answer replaces them.
#[derive(Default)]
pub struct HomeCache(HashMap<String, (Instant, CatalogHome)>);

impl HomeCache {
    /// Keeps `home` as the front page of `source`.
    pub fn put(&mut self, source: &str, home: &CatalogHome) {
        let entry = (Instant::now(), home.clone());
        self.0.insert(source.to_string(), entry);
    }

    /// The front page last seen of `source`, unless it is long out of date.
    pub fn get(&self, source: &str) -> Option<CatalogHome> {
        self.get_at(source, Instant::now())
    }

    fn get_at(&self, source: &str, now: Instant) -> Option<CatalogHome> {
        let (at, home) = self.0.get(source)?;
        (now.saturating_duration_since(*at) < HOME_LIFE).then(|| home.clone())
    }
}

/// What to ask a server for.
#[derive(Clone, Debug)]
pub enum Load {
    Home,
    /// The rows a library opens on.
    LibraryRows(String),
    /// The titles matching the text.
    Search(String),
    /// A page of a listing; past the first it adds to what is shown. A
    /// shuffle's first asks for its order too.
    Grid(CatalogQuery),
    /// What a listing's library can be filtered by.
    Facets(CatalogQuery),
    /// A series with its seasons and one season's episodes (`season`, else
    /// the one to watch next).
    Series {
        id: String,
        season: Option<String>,
    },
    Episodes {
        series: String,
        season: String,
    },
    Item {
        id: String,
    },
}

impl Load {
    /// The tab of a library page this is for.
    fn tab(&self) -> LibraryTab {
        match self {
            Load::LibraryRows(_) => LibraryTab::Recommended,
            Load::Grid(q) | Load::Facets(q) if q.types == [CatalogKind::BoxSet] => {
                LibraryTab::Collections
            }
            _ => LibraryTab::Library,
        }
    }
}

/// A mark the server keeps per item.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mark {
    Favorite,
    Played,
}

/// A server's answer to a [`Load`].
pub enum Loaded {
    Home(CatalogHome),
    LibraryRows(Vec<CatalogRow>),
    Search {
        text: String,
        results: Vec<(&'static str, Vec<CatalogItem>)>,
    },
    Grid {
        page: CatalogPage,
        append: bool,
        /// A new shuffle's order.
        order: Option<Vec<String>>,
        /// Items asked for by id (a shuffle's): the order stays.
        by_id: bool,
    },
    /// Failing quietly: the drawer says so, the page goes on.
    Facets(Result<CatalogFacets, String>),
    Series {
        series: Box<CatalogItem>,
        seasons: Vec<CatalogItem>,
        season: Option<String>,
        episodes: Vec<CatalogItem>,
        next_up: Option<Box<CatalogItem>>,
    },
    Episodes(Vec<CatalogItem>),
    Item(Box<CatalogItem>),
}

/// Asks the server (blocking: run it on a job thread).
pub fn run(src: &dyn Catalog, load: &Load) -> fp_sources::Result<Loaded> {
    Ok(match load {
        Load::Home => Loaded::Home(src.home()?),
        Load::LibraryRows(library) => Loaded::LibraryRows(src.library_rows(library)?),
        Load::Search(text) => Loaded::Search {
            text: text.clone(),
            results: src.search(text, SEARCH_LIMIT)?,
        },
        // A page of a shuffle: those items, wherever the listing starts.
        Load::Grid(q) if !q.ids.is_empty() => Loaded::Grid {
            page: src.items(&CatalogQuery {
                start: 0,
                limit: q.ids.len(),
                ..q.clone()
            })?,
            append: q.start > 0,
            order: None,
            by_id: true,
        },
        // Server shuffles differ page to page: its order is asked for once.
        Load::Grid(q) if q.sort == CatalogSort::Random => {
            let order = src.shuffled(q)?;
            let first: Vec<String> = order.iter().take(q.limit).cloned().collect();
            let items = if first.is_empty() {
                Vec::new()
            } else {
                src.items(&CatalogQuery {
                    start: 0,
                    limit: first.len(),
                    ids: first,
                    ..q.clone()
                })?
                .items
            };
            Loaded::Grid {
                page: CatalogPage {
                    total: order.len(),
                    items,
                },
                append: false,
                order: Some(order),
                by_id: true,
            }
        }
        Load::Grid(q) => Loaded::Grid {
            page: src.items(q)?,
            append: q.start > 0,
            order: None,
            by_id: false,
        },
        Load::Facets(q) => Loaded::Facets(src.facets(q).map_err(|e| super::errors::source(&e))),
        Load::Series { id, season } => {
            let series = Box::new(src.item(id)?);
            let seasons = src.seasons(id)?;
            let next_up = src
                .next_up(Some(id), 1)
                .ok()
                .and_then(|n| n.into_iter().next())
                .map(Box::new);
            let season = season
                .clone()
                .or_else(|| next_up.as_ref().and_then(|e| e.season_id.clone()))
                .or_else(|| seasons.first().map(|s| s.id.clone()));
            let episodes = src.episodes(id, season.as_deref())?;
            Loaded::Series {
                series,
                seasons,
                season,
                episodes,
                next_up,
            }
        }
        Load::Episodes { series, season } => Loaded::Episodes(src.episodes(series, Some(season))?),
        Load::Item { id } => Loaded::Item(Box::new(src.item(id)?)),
    })
}

impl CatalogView {
    /// The server's front page, loading under `seq`; `cached` is shown
    /// meanwhile.
    pub fn new(seq: u64, cached: Option<CatalogHome>) -> CatalogView {
        CatalogView {
            pages: vec![Page {
                seq,
                request_seq: None,
                loading: true,
                error: None,
                loaded_at: None,
                body: Body::Home {
                    home: cached,
                    search: String::new(),
                    typed: None,
                },
            }],
        }
    }

    /// Starts a request without changing the page's identity. Facet loads
    /// remain independent, so changing a filter cannot strand the drawer.
    pub fn begin_request(&mut self, seq: u64, request_seq: u64, load: &Load) -> Option<u64> {
        let page = self.pages.iter_mut().find(|page| page.seq == seq)?;
        if matches!(load, Load::Facets(_)) {
            if let Some(grid) = page.body.grid_mut() {
                grid.facets = Facets::Asked;
            }
            return Some(page.seq);
        }
        if let Body::Library(l) = &mut page.body {
            l.target = load.tab();
            l.asked[l.target.index()] = true;
        }
        page.request_seq = Some(request_seq);
        page.loading = true;
        Some(request_seq)
    }

    /// Gives an answer to its page, discarding superseded data loads.
    pub fn loaded(&mut self, seq: u64, result: Result<Loaded, String>) {
        let facets = matches!(&result, Ok(Loaded::Facets(_)));
        let Some(p) = self.pages.iter_mut().find(|p| {
            if facets {
                p.seq == seq
            } else {
                p.request_seq.unwrap_or(p.seq) == seq
            }
        }) else {
            return;
        };
        // The drawer's choices come beside the page's own answers.
        if !facets {
            p.loading = false;
        }
        let loaded = match result {
            Ok(l) => l,
            Err(e) => {
                p.error = Some(e);
                // A library whose rows failed shows its titles instead.
                if let Body::Library(l) = &mut p.body
                    && l.target == LibraryTab::Recommended
                    && l.tab == LibraryTab::Recommended
                    && l.rows.is_none()
                {
                    l.tab = LibraryTab::Library;
                }
                return;
            }
        };
        if !facets {
            p.error = None;
            p.loaded_at = Some(Instant::now());
        }
        match (&mut p.body, loaded) {
            (Body::Home { home, .. }, Loaded::Home(h)) => *home = Some(h),
            (Body::Library(l), Loaded::LibraryRows(rows)) => {
                l.asked[LibraryTab::Recommended.index()] = true;
                l.got[LibraryTab::Recommended.index()] = true;
                // Nothing recommended: the titles are the page.
                if rows.is_empty() && l.tab == LibraryTab::Recommended {
                    l.tab = LibraryTab::Library;
                }
                l.rows = Some(rows);
            }
            (
                Body::Library(l),
                Loaded::Grid {
                    page,
                    append,
                    order,
                    by_id,
                },
            ) => {
                let tab = l.target;
                l.asked[tab.index()] = true;
                l.got[tab.index()] = true;
                if let Some(g) = l.grid_of(tab) {
                    g.took(page, append, order, by_id);
                }
            }
            (Body::Library(l), Loaded::Facets(f)) => {
                let tab = l.tab;
                if let Some(g) = l.grid_of(tab) {
                    g.facets = match f {
                        Ok(f) => Facets::Got(f),
                        Err(e) => Facets::Failed(e),
                    };
                }
            }
            (Body::Search { asked, results, .. }, Loaded::Search { text, results: r }) => {
                *asked = text;
                *results = r;
            }
            (
                Body::Grid(g),
                Loaded::Grid {
                    page,
                    append,
                    order,
                    by_id,
                },
            ) => g.took(page, append, order, by_id),
            (Body::Grid(g), Loaded::Facets(f)) => {
                g.facets = match f {
                    Ok(f) => Facets::Got(f),
                    Err(e) => Facets::Failed(e),
                }
            }
            (
                Body::Series {
                    series,
                    seasons,
                    season,
                    episodes,
                    next_up,
                },
                Loaded::Series {
                    series: s,
                    seasons: ss,
                    season: chosen,
                    episodes: es,
                    next_up: n,
                },
            ) => {
                *series = s;
                *seasons = ss;
                *season = chosen;
                *episodes = es;
                *next_up = n;
            }
            (Body::Series { episodes, .. }, Loaded::Episodes(es)) => *episodes = es,
            (Body::Item { item }, Loaded::Item(i)) => *item = i,
            _ => {}
        }
    }

    /// What brings the page shown up to date (after watching something).
    /// Opens a page, loading until answer `seq` arrives.
    fn open(&mut self, seq: u64, body: Body) {
        self.pages.push(Page {
            seq,
            request_seq: None,
            loading: true,
            error: None,
            loaded_at: None,
            body,
        });
    }

    /// Whether the page shown was answered so lately that asking again
    /// would only repeat it.
    pub fn loaded_recently(&self) -> bool {
        self.loaded_recently_at(Instant::now())
    }

    fn loaded_recently_at(&self, now: Instant) -> bool {
        let at = self.pages.last().and_then(|p| p.loaded_at);
        at.is_some_and(|at| now.saturating_duration_since(at) < FRESH)
    }

    pub fn reload(&mut self) -> Option<(u64, Load)> {
        let p = self.pages.last_mut()?;
        p.loading = true;
        let load = match &mut p.body {
            Body::Home { .. } => Load::Home,
            Body::Search { asked, .. } => Load::Search(asked.clone()),
            // The same shuffle, brought up to date.
            Body::Grid(g) => g.reload_load(),
            Body::Library(l) => {
                // The tabs not shown ask again when they are.
                for tab in LibraryTab::ALL {
                    l.asked[tab.index()] &= tab == l.tab || !l.got[tab.index()];
                }
                l.load_for(l.tab)
            }
            Body::Series { series, season, .. } => Load::Series {
                id: series.id.clone(),
                season: season.clone(),
            },
            Body::Item { item } => Load::Item {
                id: item.id.clone(),
            },
        };
        Some((p.seq, load))
    }

    /// Shows a tab of the library page shown (`recommended`, `library` or
    /// `collections`); false when no such tab is there to show.
    pub fn show_tab(&mut self, tab: &str) -> bool {
        let tab = match tab {
            "recommended" => LibraryTab::Recommended,
            "library" => LibraryTab::Library,
            "collections" => LibraryTab::Collections,
            _ => return false,
        };
        match self.pages.last_mut().map(|p| &mut p.body) {
            Some(Body::Library(l)) => {
                l.show(tab);
                true
            }
            _ => false,
        }
    }

    /// Whether the page shown has its Filters drawer open.
    pub fn filters_open(&self) -> bool {
        match self.pages.last().map(|p| &p.body) {
            Some(Body::Grid(g)) => g.filters_open,
            Some(Body::Library(l)) => match l.tab {
                LibraryTab::Recommended => false,
                LibraryTab::Library => l.grid.filters_open,
                LibraryTab::Collections => l.collections.filters_open,
            },
            _ => false,
        }
    }

    /// Opens or closes the Filters drawer of the page shown, a grid.
    pub fn show_filters(&mut self, open: bool) {
        if let Some(g) = self.pages.last_mut().and_then(|p| p.body.grid_mut()) {
            g.filters_open = open;
        }
    }

    /// Shows a mark at once wherever the item appears; the server is told
    /// separately.
    fn mark(&mut self, id: &str, mark: Mark, on: bool) {
        let set = |i: &mut CatalogItem| {
            if i.id != id {
                return;
            }
            match mark {
                Mark::Favorite => i.favorite = on,
                Mark::Played => {
                    i.played = on;
                    i.position = 0.0;
                }
            }
        };
        for p in &mut self.pages {
            match &mut p.body {
                Body::Home { home: Some(h), .. } => {
                    for i in h
                        .resume
                        .iter_mut()
                        .chain(h.next_up.iter_mut())
                        .chain(h.latest.iter_mut().flat_map(|(_, l)| l.iter_mut()))
                        .chain(h.rows.iter_mut().flat_map(|r| r.items.iter_mut()))
                    {
                        set(i);
                    }
                }
                Body::Search { results, .. } => results
                    .iter_mut()
                    .flat_map(|(_, items)| items.iter_mut())
                    .for_each(set),
                Body::Grid(g) => g.items.iter_mut().for_each(set),
                Body::Library(l) => l.items_mut().for_each(set),
                Body::Series {
                    series,
                    episodes,
                    next_up,
                    ..
                } => {
                    set(series);
                    episodes.iter_mut().for_each(set);
                    if let Some(n) = next_up {
                        set(n);
                    }
                }
                Body::Item { item } => set(item),
                _ => {}
            }
        }
    }
}

/// What the page asked for while it was drawn.
enum Go {
    /// Into a library, collection, folder, series or season.
    Open(CatalogItem),
    /// Item `.1` of a list, the rest queued; `.2` from the start.
    Play(Vec<CatalogItem>, usize, bool),
    /// Play next (`true`) or Add to queue.
    Queue(CatalogItem, bool),
    AddToPlaylist(CatalogItem),
    /// Everything on the server with this tag.
    Tag(String),
    Details(CatalogItem),
    /// Load again what the page shows (`Load` with the page's number).
    Reload(Load),
    /// The Filters drawer's choices, beside what the page shows.
    Facets(CatalogQuery),
    Mark(String, Mark, bool),
    /// Play the item at this location from this version (`None`: the
    /// tallest that fits).
    Version(String, Option<String>),
    Search(String),
    /// A row's whole listing, under the row's title.
    SeeAll(String, CatalogQuery),
}

/// A server's page; true when Back was clicked.
pub fn page(
    ui: &mut egui::Ui,
    v: &mut View,
    source: &str,
    name: &str,
    j: &mut CatalogView,
) -> bool {
    let caps = v
        .services
        .opener
        .source(source)
        .and_then(|src| src.as_catalog().map(Catalog::capabilities))
        .unwrap_or_default();
    let subtitle = trail(&j.pages);
    let Some(p) = j.pages.last_mut() else {
        return true;
    };
    let mut refresh = false;
    let back = page::header(ui, name, Some(&subtitle), |ui| {
        refresh = widgets::icon_button(ui, icons::ARROWS_CLOCKWISE, 44.0, false)
            .tip(tr!("feed-refresh"))
            .clicked();
        if let Body::Home { search, typed, .. } | Body::Search { search, typed, .. } = &mut p.body
            && page::search_field(ui, search, tr!("jf-search-server"), 220.0)
        {
            *typed = Some(Instant::now());
        }
    });
    if refresh {
        if let Some((seq, load)) = j.reload() {
            v.actions.push(Action::Catalog {
                source: source.to_string(),
                seq,
                load,
            });
        }
        return back;
    }
    let Some(p) = j.pages.last_mut() else {
        return true;
    };
    page::status(ui, p.loading && !has_content(&p.body), p.error.as_deref());
    let loading = p.loading;
    let top = p
        .body
        .grid_mut()
        .is_some_and(|g| std::mem::take(&mut g.to_top));
    let mut go: Option<Go> = None;
    page::scroll_from(ui, top, |ui| {
        go = match &mut p.body {
            Body::Home {
                home,
                search,
                typed,
            } => {
                if let Some(t) = *typed
                    && t.elapsed() >= SEARCH_DELAY
                {
                    *typed = None;
                    (!search.trim().is_empty()).then(|| Go::Search(std::mem::take(search)))
                } else {
                    if typed.is_some() {
                        ui.ctx().request_repaint_after(SEARCH_DELAY);
                    }
                    home.as_ref().and_then(|h| home_rows(ui, v, h))
                }
            }
            Body::Search {
                search,
                typed,
                asked,
                results,
            } => search_results(ui, v, search, typed, asked, results, loading),
            Body::Grid(g) => grid(ui, v, g, loading, caps),
            Body::Library(l) => library_page(ui, v, l, loading, caps),
            Body::Series {
                series,
                seasons,
                season,
                episodes,
                next_up,
            } => series_page(
                ui,
                v,
                series,
                seasons,
                season,
                episodes,
                next_up.as_deref(),
                caps,
            ),
            Body::Item { item } => details(ui, v, item, caps),
        };
    });
    if let Some(go) = go {
        follow(v, source, j, caps, go);
    }
    back
}

/// Carries out what the page asked for.
fn follow(v: &mut View, source: &str, j: &mut CatalogView, caps: CatalogCapabilities, go: Go) {
    let ask = |v: &mut View, seq: u64, load: Load| {
        v.actions.push(Action::Catalog {
            source: source.to_string(),
            seq,
            load,
        });
    };
    match go {
        Go::Reload(load) => {
            if let Some(p) = j.pages.last_mut() {
                p.loading = true;
                ask(v, p.seq, load);
            }
        }
        Go::Facets(query) => {
            if let Some(p) = j.pages.last() {
                ask(v, p.seq, Load::Facets(query));
            }
        }
        Go::Play(list, i, from_start) => {
            let reqs: Vec<OpenRequest> = list
                .iter()
                .enumerate()
                .map(|(k, item)| request(source, item, from_start && k == i))
                .collect();
            v.actions.push(Action::OpenList(reqs, i));
        }
        Go::Queue(item, next) => v.actions.push(Action::Queue {
            req: request(source, &item, false),
            next,
        }),
        Go::AddToPlaylist(item) => {
            v.state.picking = Some(super::Picking {
                reqs: vec![request(source, &item, false)],
                title: item.name.clone(),
            });
        }
        Go::Mark(id, mark, on) => {
            j.mark(&id, mark, on);
            v.actions.push(Action::CatalogMark {
                source: source.to_string(),
                id,
                mark,
                on,
            });
        }
        Go::Version(location, version) => v.actions.push(Action::CatalogVersion {
            source: source.to_string(),
            location,
            version,
        }),
        Go::Details(item) => {
            let seq = v.state.next_seq();
            ask(
                v,
                seq,
                Load::Item {
                    id: item.id.clone(),
                },
            );
            j.open(
                seq,
                Body::Item {
                    item: Box::new(item),
                },
            );
        }
        Go::Search(text) => {
            let seq = v.state.next_seq();
            let text = text.trim().to_string();
            ask(v, seq, Load::Search(text.clone()));
            j.open(
                seq,
                Body::Search {
                    search: text.clone(),
                    typed: None,
                    asked: text,
                    results: Vec::new(),
                },
            );
        }
        Go::Open(item) => {
            let seq = v.state.next_seq();
            let (load, body) = page_for(item, caps);
            ask(v, seq, load);
            j.open(seq, body);
        }
        Go::SeeAll(title, query) => {
            let seq = v.state.next_seq();
            let (load, body) = see_all_page(title, query);
            ask(v, seq, load);
            j.open(seq, body);
        }
        Go::Tag(tag) => {
            let seq = v.state.next_seq();
            let query = CatalogQuery {
                limit: PAGE,
                ..CatalogQuery::tagged(&tag)
            };
            let title = tr!("jf-tagged", tag = tag.as_str());
            let body = Body::Grid(Box::new(Grid::new(title, query.clone())));
            ask(v, seq, Load::Grid(query));
            j.open(seq, body);
        }
    }
}

/// What a row's "See all" loads, and the grid that shows it.
fn see_all_page(title: String, query: CatalogQuery) -> (Load, Body) {
    let query = CatalogQuery {
        start: 0,
        limit: PAGE,
        ..query
    };
    let body = Body::Grid(Box::new(Grid::new(title, query.clone())));
    (Load::Grid(query), body)
}

/// What opening a library, collection, folder, series or season loads,
/// and the page that shows it. A server with library pages opens a
/// library on its rows; any other shows its titles.
fn page_for(item: CatalogItem, caps: CatalogCapabilities) -> (Load, Body) {
    match item.kind {
        CatalogKind::Library if caps.library_pages => {
            let page = LibraryPage::new(item);
            let load = page.load_for(LibraryTab::Recommended);
            (load, Body::Library(Box::new(page)))
        }
        CatalogKind::Series | CatalogKind::Season => {
            let (id, season) = if item.kind == CatalogKind::Season {
                (
                    item.series_id.clone().unwrap_or_default(),
                    Some(item.id.clone()),
                )
            } else {
                (item.id.clone(), None)
            };
            let load = Load::Series {
                id: id.clone(),
                season: season.clone(),
            };
            let series = CatalogItem {
                id,
                name: item.series_name.clone().unwrap_or(item.name.clone()),
                kind: CatalogKind::Series,
                ..item
            };
            let body = Body::Series {
                series: Box::new(series),
                seasons: Vec::new(),
                season,
                episodes: Vec::new(),
                next_up: None,
            };
            (load, body)
        }
        _ => {
            let query = CatalogQuery {
                limit: PAGE,
                ..CatalogQuery::children(&item)
            };
            let body = Body::Grid(Box::new(Grid::new(item.name.clone(), query.clone())));
            (Load::Grid(query), body)
        }
    }
}

/// The page a card of another screen (a server's playlist or history) opens
/// on the server: a video's details, else what `page_for` makes of it.
pub fn open_page(item: CatalogItem, caps: CatalogCapabilities) -> (Load, Body) {
    if item.kind.playable() {
        let load = Load::Item {
            id: item.id.clone(),
        };
        let body = Body::Item {
            item: Box::new(item),
        };
        (load, body)
    } else {
        page_for(item, caps)
    }
}

/// Opens `body` on top of the pages shown, loading until answer `seq`.
pub fn push_page(j: &mut CatalogView, seq: u64, body: Body) {
    j.open(seq, body);
}

/// Where the page shown is on the server, as the header's second line: the
/// pages opened to reach it ("Movies › Blade Runner", the last three), or
/// what the server is on its front page.
fn trail(pages: &[Page]) -> String {
    let parts: Vec<String> = pages
        .iter()
        .filter_map(|p| match &p.body {
            Body::Home { .. } => None,
            Body::Search { asked, .. } => Some(tr!("jf-trail-search", text = asked.as_str())),
            Body::Grid(g) => Some(g.title.clone()),
            Body::Library(l) => Some(l.library.name.clone()),
            Body::Series { series, .. } => Some(series.name.clone()),
            Body::Item { item } => Some(item.name.clone()),
        })
        .collect();
    if parts.is_empty() {
        return String::new();
    }
    parts[parts.len().saturating_sub(3)..].join(&format!("  {}  ", icons::CARET_RIGHT))
}

/// The page shows something already (a reload keeps it on screen).
fn has_content(body: &Body) -> bool {
    match body {
        Body::Home { home, .. } => home.is_some(),
        Body::Search { results, .. } => !results.is_empty(),
        Body::Grid(g) => !g.items.is_empty(),
        Body::Library(l) => match l.tab {
            LibraryTab::Recommended => l.rows.is_some(),
            LibraryTab::Library => !l.grid.items.is_empty(),
            LibraryTab::Collections => !l.collections.items.is_empty(),
        },
        Body::Series { episodes, .. } => !episodes.is_empty(),
        Body::Item { .. } => true,
    }
}

/// Opens `item`, starting from the server's resume point.
pub(super) fn request(source: &str, item: &CatalogItem, from_start: bool) -> OpenRequest {
    OpenRequest {
        location: item.location.clone(),
        source_id: Some(source.to_string()),
        entry: Some(item.to_entry()),
        title: None,
        start_at: if from_start {
            Some(0.0)
        } else {
            (item.position > 1.0).then_some(item.position)
        },
        ..Default::default()
    }
}

/// "1982 · 1:57:00", "S1:E3 · Westworld", "4 seasons".
fn card_facts(item: &CatalogItem) -> String {
    let mut f = Vec::new();
    match item.kind {
        CatalogKind::Episode => {
            if let Some(e) = item.episode_label() {
                f.push(e);
            }
            if let Some(s) = &item.series_name {
                f.push(s.clone());
            }
        }
        CatalogKind::Series => {
            if let Some(y) = item.year {
                f.push(y.to_string());
            }
            match item.unplayed {
                Some(n) if n > 0 => f.push(tr!("jf-unwatched", count = n)),
                _ => {}
            }
        }
        CatalogKind::BoxSet
        | CatalogKind::Folder
        | CatalogKind::Library
        | CatalogKind::Playlist => {
            if let Some(n) = item.child_count {
                f.push(tr!("count-items", count = (n as usize)));
            }
        }
        _ => {
            if let Some(y) = item.year {
                f.push(y.to_string());
            }
        }
    }
    if item.kind.playable()
        && let Some(d) = item.runtime
    {
        f.push(fmt_time(d));
    }
    f.join("  ·  ")
}

/// A VR format worth a badge (flat films get none).
fn vr_badge(item: &CatalogItem) -> Option<String> {
    format_badge(item.format)
}

/// `format` when it is VR or 3D.
fn format_badge(format: Option<fp_core::format::VideoFormat>) -> Option<String> {
    let f = format?;
    let flat = f.projection == fp_core::format::Projection::Flat
        && f.stereo == fp_core::format::StereoLayout::Mono;
    (!flat).then(|| widgets::format_short(&f))
}

/// An item's card, a poster or 16:9 art.
pub(super) fn item_card(
    ui: &mut egui::Ui,
    v: &mut View,
    item: &CatalogItem,
    portrait: bool,
) -> page::CardResponse {
    let thumb = if portrait {
        item.poster.clone()
    } else {
        item.landscape.clone().or_else(|| item.poster.clone())
    };
    let facts = card_facts(item);
    let badge = vr_badge(item);
    let placeholder = match item.kind {
        CatalogKind::Series | CatalogKind::Season => icons::MONITOR_PLAY,
        CatalogKind::Library | CatalogKind::Folder | CatalogKind::BoxSet => icons::FOLDERS,
        CatalogKind::Playlist => icons::PLAYLIST,
        CatalogKind::Person => icons::USER,
        _ => icons::FILM_STRIP,
    };
    page::card(
        ui,
        v.thumbs,
        &Card {
            size: if portrait {
                page::PORTRAIT
            } else {
                page::LANDSCAPE
            },
            thumb: thumb.as_deref(),
            placeholder,
            title: &item.name,
            facts: &facts,
            badge: badge.as_deref(),
            progress: item.progress(),
            watched: item.played,
            favorite: item.favorite,
            more: item.kind.playable(),
            play: item.kind.playable(),
            ..Card::default()
        },
    )
}

/// What clicking a card in `list` does. Played `alone`, a card plays only
/// itself (Continue Watching and a server's rows: what follows it there is
/// not what comes next); otherwise the rest of the list is queued after it.
fn card_go(c: page::CardResponse, list: &[CatalogItem], i: usize, alone: bool) -> Option<Go> {
    let item = &list[i];
    if c.more {
        Some(Go::Details(item.clone()))
    } else if c.open && item.kind.playable() {
        if alone {
            Some(Go::Play(vec![item.clone()], 0, false))
        } else {
            Some(Go::Play(list.to_vec(), i, false))
        }
    } else if c.open {
        Some(Go::Open(item.clone()))
    } else {
        None
    }
}

/// A titled row of cards as a page draws it.
struct Shelf<'a> {
    /// Keys its scroll position.
    id: String,
    title: String,
    items: &'a [CatalogItem],
    /// One shape for the whole row; `None` lets each card pick its own.
    shape: Option<RowShape>,
    /// What "See all" opens, when the server has more.
    more: Option<&'a CatalogQuery>,
    /// Its cards play alone, see [`card_go`].
    alone: bool,
}

impl<'a> Shelf<'a> {
    fn new(id: &str, title: &str, items: &'a [CatalogItem], shape: Option<RowShape>) -> Self {
        Shelf {
            id: id.to_string(),
            title: title.to_string(),
            items,
            shape,
            more: None,
            alone: false,
        }
    }

    /// Whether card `i` is a poster.
    fn portrait(&self, i: usize) -> bool {
        match self.shape {
            Some(shape) => shape == RowShape::Portrait,
            None => self.items[i].kind.portrait(),
        }
    }
}

/// The rows of a server's front page in the order they are drawn: My
/// Media, Continue Watching, then the server's own rows when it sent any,
/// else the next episodes and the newest in each library.
fn home_shelves(h: &CatalogHome) -> Vec<Shelf<'_>> {
    let landscape = Some(RowShape::Landscape);
    let my_media = Shelf::new("my-media", tr!("jf-my-media"), &h.libraries, landscape);
    let mut resume = Shelf::new("continue", tr!("jf-continue"), &h.resume, landscape);
    resume.alone = true;
    let mut shelves = vec![my_media, resume];
    if h.rows.is_empty() {
        let next_up = Shelf::new("next-up", tr!("jf-next-up"), &h.next_up, landscape);
        shelves.push(next_up);
        for (library, items) in &h.latest {
            let title = tr!("jf-latest-in", library = library.name.as_str());
            let id = format!("latest:{}", library.id);
            shelves.push(Shelf::new(&id, &title, items, None));
        }
    } else {
        shelves.extend(h.rows.iter().map(row_shelf));
    }
    shelves
}

/// A server's row, drawn in the shape it chose.
fn row_shelf(row: &CatalogRow) -> Shelf<'_> {
    let id = format!("row:{}", row.id);
    let mut shelf = Shelf::new(&id, &row.title, &row.items, Some(row.shape));
    shelf.more = row.more.as_ref();
    shelf.alone = true;
    shelf
}

/// Draws a row; what clicking it asks for.
fn shelf_view(ui: &mut egui::Ui, v: &mut View, shelf: &Shelf<'_>) -> Option<Go> {
    let mut go = None;
    let label = shelf.more.map(|_| tr!("catalog-see-all"));
    let (items, alone) = (shelf.items, shelf.alone);
    let (id, title, n) = (&shelf.id, &shelf.title, items.len());
    let all = page::carousel_keyed(ui, id, title, None, n, label, |ui, i| {
        let c = item_card(ui, v, &items[i], shelf.portrait(i));
        go = go.take().or_else(|| card_go(c, items, i, alone));
    });
    match (go, shelf.more) {
        (None, Some(query)) if all => Some(Go::SeeAll(shelf.title.clone(), query.clone())),
        (go, _) => go,
    }
}

/// A titled row of cards, each in the shape its kind has.
fn item_row(ui: &mut egui::Ui, v: &mut View, title: &str, items: &[CatalogItem]) -> Option<Go> {
    shelf_view(ui, v, &Shelf::new(title, title, items, None))
}

fn home_rows(ui: &mut egui::Ui, v: &mut View, h: &CatalogHome) -> Option<Go> {
    ui.add_space(6.0);
    let mut go = None;
    for shelf in home_shelves(h) {
        go = go.or(shelf_view(ui, v, &shelf));
    }
    if h.libraries.is_empty() {
        page::empty_state(
            ui,
            icons::MONITOR_PLAY,
            tr!("jf-no-libraries"),
            tr!("jf-no-libraries-desc"),
            |_| {},
        );
    }
    go
}

/// A library page: its tabs, then the one chosen.
fn library_page(
    ui: &mut egui::Ui,
    v: &mut View,
    l: &mut LibraryPage,
    loading: bool,
    caps: CatalogCapabilities,
) -> Option<Go> {
    ui.add_space(4.0);
    let tabs = l.tabs();
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 8.0;
        for (tab, label) in tabs {
            if widgets::chip(ui, label, l.tab == tab).clicked() && l.tab != tab {
                l.show(tab);
            }
        }
    });
    ui.add_space(10.0);
    // A tab shown for the first time asks the server now.
    if let Some(load) = l.next_load(loading) {
        return Some(Go::Reload(load));
    }
    match l.tab {
        LibraryTab::Recommended => {
            let rows = l.rows.as_deref().unwrap_or_default();
            library_rows(ui, v, &l.library.id, rows)
        }
        LibraryTab::Library => grid(ui, v, &mut l.grid, loading, caps),
        LibraryTab::Collections => {
            let go = grid(ui, v, &mut l.collections, loading, caps);
            let none = l.got[LibraryTab::Collections.index()] && l.collections.items.is_empty();
            if none && !loading {
                page::empty_state(
                    ui,
                    icons::FOLDERS,
                    tr!("catalog-no-collections"),
                    tr!("catalog-no-collections-desc"),
                    |_| {},
                );
            }
            go
        }
    }
}

/// The rows of a library's Recommended tab; their scroll positions are the
/// library's own.
fn library_rows(ui: &mut egui::Ui, v: &mut View, library: &str, rows: &[CatalogRow]) -> Option<Go> {
    ui.add_space(6.0);
    let mut go = None;
    for row in rows {
        let mut shelf = row_shelf(row);
        shelf.id = format!("{library}/{}", shelf.id);
        go = go.or(shelf_view(ui, v, &shelf));
    }
    go
}

/// Search results, a row per kind. Typing in the header's search field
/// searches again on this page.
fn search_results(
    ui: &mut egui::Ui,
    v: &mut View,
    search: &str,
    typed: &mut Option<Instant>,
    asked: &str,
    results: &[(&'static str, Vec<CatalogItem>)],
    loading: bool,
) -> Option<Go> {
    if let Some(t) = *typed {
        if t.elapsed() >= SEARCH_DELAY {
            *typed = None;
            let text = search.trim();
            if !text.is_empty() && text != asked {
                return Some(Go::Reload(Load::Search(text.to_string())));
            }
        } else {
            ui.ctx().request_repaint_after(SEARCH_DELAY);
        }
    }
    ui.add_space(6.0);
    let mut go = None;
    for (section, items) in results {
        go = go.or(item_row(ui, v, section, items));
    }
    if results.is_empty() && !loading {
        page::empty_state(
            ui,
            icons::MAGNIFYING_GLASS,
            tr!("jf-nothing-found"),
            &tr!("jf-nothing-found-desc", text = asked),
            |_| {},
        );
    }
    go
}

/// The orders a listing offers, the server's own first when it began in it.
fn sorts(server_order: bool) -> Vec<(CatalogSort, &'static str)> {
    let mut sorts = Vec::new();
    if server_order {
        sorts.push((CatalogSort::Server, tr!("sort-server-order")));
    }
    sorts.extend([
        (CatalogSort::Name, tr!("sort-title")),
        (CatalogSort::Added, tr!("sort-date-added")),
        (CatalogSort::Released, tr!("sort-release-date")),
        (CatalogSort::Rating, tr!("filter-rating")),
        (CatalogSort::Runtime, tr!("filter-length")),
        (CatalogSort::LastWatched, tr!("sort-last-watched")),
        (CatalogSort::Random, tr!("sort-shuffle")),
    ]);
    sorts
}

/// A searchable, sortable, filterable listing, loading more as its end
/// shows.
fn grid(
    ui: &mut egui::Ui,
    v: &mut View,
    g: &mut Grid,
    loading: bool,
    caps: CatalogCapabilities,
) -> Option<Go> {
    let mut changed = false;
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 8.0;
        if page::search_field(ui, &mut g.query.search, tr!("jf-search"), 220.0) {
            g.typed = Some(Instant::now());
        }
        if page::filters_button(ui, g.on().len()).clicked() {
            g.filters_open = !g.filters_open;
        }
        ui.add_space(10.0);
        for (f, label) in [
            (CatalogFilter::All, tr!("filter-all")),
            (CatalogFilter::Unplayed, tr!("filter-unwatched")),
            (CatalogFilter::Resumable, tr!("filter-in-progress")),
            (CatalogFilter::Favorites, tr!("filter-favorites")),
        ] {
            if f == CatalogFilter::Favorites && !caps.favorites {
                continue;
            }
            if widgets::chip(ui, label, g.query.filter == f).clicked() && g.query.filter != f {
                g.query.filter = f;
                changed = true;
            }
        }
    });
    ui.add_space(6.0);
    ui.horizontal(|ui| {
        if page::sort_bar(ui, &sorts(g.server_order), &mut g.query.sort) {
            g.query.descending = g.query.sort.default_descending();
            changed = true;
        }
        changed |= match g.query.sort {
            CatalogSort::Random => page::shuffle_again(ui),
            // The server's order has no other way round.
            CatalogSort::Server => false,
            _ => page::direction(
                ui,
                &mut g.query.descending,
                [tr!("sort-largest-first"), tr!("sort-smallest-first")],
            ),
        };
        page::count_label(ui, &tr!("count-items", count = g.total));
    });
    match page::active_filters(ui, &g.labels()) {
        Some(page::FilterEdit::Remove(i)) => {
            g.take_off(i);
            changed = true;
        }
        Some(page::FilterEdit::Clear) => {
            g.clear_filters();
            changed = true;
        }
        None => {}
    }
    ui.add_space(10.0);
    if let Some(t) = g.typed {
        if t.elapsed() >= SEARCH_DELAY {
            g.typed = None;
            changed = true;
        } else {
            ui.ctx().request_repaint_after(SEARCH_DELAY);
        }
    }
    let mut go = None;
    if g.filters_open {
        if let Some(query) = g.unasked_facets() {
            go = Some(Go::Facets(query));
        }
        let mut open = true;
        let count = tr!("count-items", count = g.total);
        let clear = page::filter_drawer(ui, &mut open, &count, |ui| changed |= drawer(ui, g, caps));
        g.filters_open = open;
        if clear {
            g.clear_filters();
            changed = true;
        }
    }
    if changed {
        return Some(Go::Reload(Load::Grid(g.restart())));
    }
    if g.items.is_empty() {
        return go;
    }
    let items = &g.items;
    let portrait = items.iter().filter(|i| i.kind.portrait()).count() * 2 >= items.len();
    let width = if portrait {
        page::PORTRAIT.x
    } else {
        page::LANDSCAPE.x
    };
    page::grid(ui, items.len(), width, |ui, i| {
        let c = item_card(ui, v, &items[i], portrait);
        go = go.take().or_else(|| card_go(c, items, i, false));
    });
    if items.len() < g.total && page::load_more(ui, loading) {
        go = Some(Go::Reload(Load::Grid(g.next_page())));
    }
    go
}

/// The Filters drawer's sections, those the library has choices for; true
/// when a filter changed.
fn drawer(ui: &mut egui::Ui, g: &mut Grid, caps: CatalogCapabilities) -> bool {
    let facets = match std::mem::replace(&mut g.facets, Facets::Asked) {
        Facets::Got(f) => f,
        other => {
            match &other {
                Facets::Failed(e) => {
                    ui.label(
                        RichText::new(format!("{}  {e}", icons::WARNING_CIRCLE))
                            .color(theme::ERROR),
                    );
                }
                _ => {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label(RichText::new(tr!("jf-asking")).color(theme::TEXT_2));
                    });
                }
            }
            g.facets = other;
            return false;
        }
    };
    let mut changed = false;
    let toggle = |list: &mut Vec<String>, name: &str| match list.iter().position(|x| x == name) {
        Some(i) => {
            list.remove(i);
        }
        None => list.push(name.to_string()),
    };
    let q = &mut g.query;
    let lists = &mut g.lists;
    let mut list = |ui: &mut egui::Ui,
                    title: &'static str,
                    hint: &str,
                    names: &[&str],
                    chosen: &mut Vec<String>| {
        if names.is_empty() {
            return false;
        }
        let choices: Vec<(&str, Option<usize>)> = names.iter().map(|n| (*n, None)).collect();
        let state = lists.entry(title).or_default();
        let on = |n: &str| chosen.iter().any(|c| c == n);
        let picked = page::filter_list(ui, title, hint, &choices, on, state);
        if let Some(name) = picked {
            toggle(chosen, name);
        }
        picked.is_some()
    };
    let genres: Vec<&str> = facets.genres.iter().map(String::as_str).collect();
    changed |= list(
        ui,
        tr!("filter-genres"),
        tr!("filter-find-genre"),
        &genres,
        &mut q.genres,
    );
    let decades = decades(&facets.years);
    if !decades.is_empty() {
        let on = decades_on(&q.years);
        page::filter_section(ui, tr!("filter-released"), |ui| {
            for d in decades {
                if page::filter_chip(ui, &tr!("filter-decade", decade = d), None, on.contains(&d)) {
                    if on.contains(&d) {
                        q.years.retain(|y| !(d..d + 10).contains(y));
                    } else {
                        q.years.extend(d..d + 10);
                    }
                    changed = true;
                }
            }
        });
    }
    // Series and folders have no width of their own.
    let videos = caps.width_filter && (q.types.is_empty() || q.types.iter().any(|t| t.playable()));
    if videos {
        page::filter_section(ui, tr!("adjust-resolution"), |ui| {
            for (w, label) in RESOLUTIONS {
                let on = q.min_width == Some(w);
                if page::filter_chip(ui, label, None, on) {
                    q.min_width = (!on).then_some(w);
                    changed = true;
                }
            }
        });
    }
    let studios: Vec<&str> = facets.studios.iter().map(String::as_str).collect();
    changed |= list(
        ui,
        tr!("filter-studios"),
        tr!("filter-find-studio"),
        &studios,
        &mut q.studios,
    );
    // The page's own tag is no choice.
    let tags: Vec<&str> = facets
        .tags
        .iter()
        .filter(|t| caps.tags && !g.base_tags.contains(t))
        .map(String::as_str)
        .collect();
    changed |= list(
        ui,
        tr!("filter-tags"),
        tr!("filter-find-tag"),
        &tags,
        &mut q.tags,
    );
    if facets.genres.is_empty() && facets.studios.is_empty() && tags.is_empty() && !videos {
        ui.label(RichText::new(tr!("jf-no-filters")).color(theme::TEXT_2));
    }
    g.facets = Facets::Got(facets);
    changed
}

/// Year, rating and other facts about an item.
fn item_facts(item: &CatalogItem) -> Vec<String> {
    let mut f = Vec::new();
    if let Some(y) = item.year {
        f.push(y.to_string());
    }
    if item.kind.playable()
        && let Some(d) = item.runtime
    {
        f.push(fmt_time(d));
    }
    if let Some(r) = &item.official_rating {
        f.push(r.clone());
    }
    if let Some(r) = item.community_rating {
        f.push(page::rating(r));
    }
    if let Some(b) = format_badge(item.shown_format()) {
        f.push(b);
    }
    if item.played {
        f.push(tr!("jf-watched").into());
    }
    f
}

/// The favourite and watched toggles.
fn mark_chips(ui: &mut egui::Ui, item: &CatalogItem, caps: CatalogCapabilities) -> Option<Go> {
    let mut go = None;
    let heart = if item.favorite {
        tr!("feed-favorite")
    } else {
        tr!("feed-add-favorite")
    };
    if caps.favorites && widgets::chip_icon(ui, Some(icons::HEART), heart, item.favorite).clicked()
    {
        go = Some(Go::Mark(item.id.clone(), Mark::Favorite, !item.favorite));
    }
    if widgets::chip_icon(ui, Some(icons::CHECK), tr!("jf-watched"), item.played).clicked() {
        go = Some(Go::Mark(item.id.clone(), Mark::Played, !item.played));
    }
    go
}

/// The start of a long text, cut at a word.
fn clip(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let cut: String = text.chars().take(max).collect();
    let cut = cut.rsplit_once(' ').map(|(a, _)| a).unwrap_or(&cut);
    format!("{}…", cut.trim_end_matches([',', '.', ';']))
}

#[allow(clippy::too_many_arguments)]
fn series_page(
    ui: &mut egui::Ui,
    v: &mut View,
    series: &CatalogItem,
    seasons: &[CatalogItem],
    season: &mut Option<String>,
    episodes: &[CatalogItem],
    next_up: Option<&CatalogItem>,
    caps: CatalogCapabilities,
) -> Option<Go> {
    let mut go = None;
    ui.add_space(4.0);
    let key = series
        .backdrop
        .clone()
        .or_else(|| series.landscape.clone())
        .or_else(|| series.poster.clone());
    page::banner(ui, v.thumbs, key.as_deref(), BANNER, |inner| {
        inner.add_space(14.0);
        let room = inner.available_width();
        widgets::title_line(
            inner,
            &series.name,
            theme::font(Weight::Bold, 40.0),
            Color32::WHITE,
            room,
        );
        let mut f = item_facts(series);
        if !seasons.is_empty() {
            f.push(tr!("count-seasons", count = seasons.len()));
        }
        page::facts(inner, &f);
        if let Some(o) = &series.overview {
            inner.add_space(4.0);
            page::prose(inner, &clip(o, 220));
        }
        inner.add_space(14.0);
        inner.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 12.0;
            if let Some(n) = next_up {
                let ep = n.episode_label().unwrap_or_default();
                let label = if n.position > 1.0 {
                    tr!("jf-resume-episode", episode = ep.as_str())
                } else {
                    tr!("jf-play-episode", episode = ep.as_str())
                };
                if widgets::button(ui, Some(icons::PLAY), &label, Kind::Primary).clicked() {
                    // The next episode, with the rest of its season after it
                    // when it is the season shown.
                    go = Some(match episodes.iter().position(|e| e.id == n.id) {
                        Some(i) => Go::Play(episodes.to_vec(), i, false),
                        None => Go::Play(vec![n.clone()], 0, false),
                    });
                }
            }
            go = go.take().or(mark_chips(ui, series, caps));
        });
    });
    ui.add_space(18.0);
    if seasons.len() > 1 {
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = Vec2::new(8.0, 8.0);
            for s in seasons {
                let selected = season.as_deref() == Some(s.id.as_str());
                if widgets::chip(ui, &s.name, selected).clicked() && !selected {
                    *season = Some(s.id.clone());
                    go = Some(Go::Reload(Load::Episodes {
                        series: series.id.clone(),
                        season: s.id.clone(),
                    }));
                }
            }
        });
        ui.add_space(12.0);
    }
    for (i, e) in episodes.iter().enumerate() {
        let (play, more) = episode_row(ui, v, e);
        if play {
            go = Some(Go::Play(episodes.to_vec(), i, false));
        }
        if more {
            go = Some(Go::Details(e.clone()));
        }
    }
    ui.add_space(20.0);
    go
}

/// One episode: still, number and title, facts, the start of its story;
/// returns (play clicked, details clicked).
fn episode_row(ui: &mut egui::Ui, v: &mut View, e: &CatalogItem) -> (bool, bool) {
    let (rect, resp) =
        ui.allocate_exact_size(Vec2::new(ui.available_width(), 132.0), Sense::click());
    if !ui.is_rect_visible(rect) {
        return (false, false);
    }
    let t = ui
        .ctx()
        .animate_bool_with_time(resp.id, resp.hovered(), 0.12);
    ui.painter().rect_filled(
        rect,
        egui::CornerRadius::same(14),
        theme::SURFACE.gamma_multiply(t),
    );
    let thumb =
        egui::Rect::from_min_size(rect.min + Vec2::new(10.0, 12.0), Vec2::new(192.0, 108.0));
    page::picture(
        ui,
        v.thumbs,
        e.landscape.as_deref().or(e.poster.as_deref()),
        thumb,
        10,
        icons::FILM_STRIP,
    );
    let painter = ui.painter();
    if let Some(p) = e.progress().filter(|p| *p > 0.005) {
        let bar = egui::Rect::from_min_max(
            egui::pos2(thumb.left() + 8.0, thumb.bottom() - 8.0),
            egui::pos2(thumb.right() - 8.0, thumb.bottom() - 4.0),
        );
        painter.rect_filled(
            bar,
            egui::CornerRadius::same(2),
            Color32::from_black_alpha(140),
        );
        let mut done = bar;
        done.set_width(bar.width() * p);
        painter.rect_filled(done, egui::CornerRadius::same(2), theme::ACCENT);
    }
    if t > 0.01 {
        painter.circle_filled(
            thumb.center(),
            24.0,
            Color32::from_black_alpha((t * 150.0) as u8),
        );
        widgets::play_mark(
            painter,
            thumb.center(),
            20.0,
            Color32::from_white_alpha((t * 255.0) as u8),
        );
    }
    let left = thumb.right() + 18.0;
    let width = rect.right() - left - 70.0;
    let text = |s: String, font: egui::FontId, color: Color32, rows: usize| {
        let mut job =
            egui::text::LayoutJob::single_section(s, egui::TextFormat::simple(font, color));
        job.wrap = egui::text::TextWrapping {
            max_width: width,
            max_rows: rows,
            break_anywhere: false,
            overflow_character: Some('…'),
        };
        ui.fonts(|f| f.layout_job(job))
    };
    let title = match e.index {
        Some(n) => format!("{n}. {}", e.name),
        None => e.name.clone(),
    };
    let mut facts = Vec::new();
    if let Some(d) = e.runtime {
        facts.push(fmt_time(d));
    }
    if e.position > 1.0
        && let Some(d) = e.runtime
    {
        let left = fmt_time(d - e.position);
        facts.push(tr!("jf-time-left", time = left));
    }
    if let Some(b) = vr_badge(e) {
        facts.push(b);
    }
    let title = text(title, theme::font(Weight::SemiBold, 18.0), theme::TEXT, 1);
    let facts = text(
        facts.join("  ·  "),
        theme::font(Weight::Regular, 15.0),
        theme::TEXT_2,
        1,
    );
    let story = text(
        e.overview.clone().unwrap_or_default(),
        theme::font(Weight::Regular, 15.0),
        theme::TEXT_3,
        2,
    );
    let painter = ui.painter();
    painter.galley(egui::pos2(left, rect.top() + 14.0), title, theme::TEXT);
    painter.galley(egui::pos2(left, rect.top() + 40.0), facts, theme::TEXT_2);
    painter.galley(egui::pos2(left, rect.top() + 64.0), story, theme::TEXT_3);
    if e.played {
        painter.text(
            egui::pos2(rect.right() - 60.0, rect.center().y),
            Align2::CENTER_CENTER,
            icons::CHECK_CIRCLE,
            theme::icon_fill(26.0),
            theme::ACCENT_HOVER,
        );
    }
    let more = egui::Rect::from_center_size(
        egui::pos2(rect.right() - 24.0, rect.center().y),
        Vec2::splat(36.0),
    );
    let more_resp = ui.interact(more, resp.id.with("more"), Sense::click());
    let m = ui
        .ctx()
        .animate_bool_with_time(more_resp.id, more_resp.hovered(), 0.12);
    let painter = ui.painter();
    painter.circle_filled(
        more.center(),
        18.0,
        Color32::from_white_alpha((m * 20.0) as u8),
    );
    painter.text(
        more.center(),
        Align2::CENTER_CENTER,
        icons::DOTS_THREE,
        theme::icon(24.0),
        theme::TEXT_2.lerp_to_gamma(Color32::WHITE, m.max(t * 0.6)),
    );
    let more_clicked = more_resp.tip(tr!("jf-details")).clicked();
    (resp.clicked() && !more_clicked, more_clicked)
}

/// A film's, episode's or video's page.
fn details(
    ui: &mut egui::Ui,
    v: &mut View,
    item: &mut CatalogItem,
    caps: CatalogCapabilities,
) -> Option<Go> {
    let mut go = None;
    ui.add_space(4.0);
    let key = item
        .backdrop
        .clone()
        .or_else(|| item.landscape.clone())
        .or_else(|| item.poster.clone());
    page::banner(ui, v.thumbs, key.as_deref(), BANNER, |inner| {
        inner.add_space(14.0);
        if let (Some(series), Some(ep)) = (&item.series_name, item.episode_label()) {
            widgets::section_label(inner, &format!("{series} · {ep}"));
        }
        let room = inner.available_width();
        widgets::title_line(
            inner,
            &item.name,
            theme::font(Weight::Bold, 40.0),
            Color32::WHITE,
            room,
        );
        page::facts(inner, &item_facts(item));
        inner.add_space(16.0);
        inner.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 12.0;
            let resume = item.position > 1.0;
            let label = if resume {
                tr!("jf-resume-from", time = fmt_time(item.position))
            } else {
                tr!("player-play").to_string()
            };
            if widgets::button(ui, Some(icons::PLAY), &label, Kind::Primary).clicked() {
                go = Some(Go::Play(vec![item.clone()], 0, false));
            }
            if resume
                && widgets::button(
                    ui,
                    Some(icons::ARROW_COUNTER_CLOCKWISE),
                    tr!("jf-start-over"),
                    Kind::Secondary,
                )
                .clicked()
            {
                go = Some(Go::Play(vec![item.clone()], 0, true));
            }
        });
        if matches!(
            item.kind,
            CatalogKind::Movie
                | CatalogKind::Episode
                | CatalogKind::Video
                | CatalogKind::MusicVideo
        ) {
            inner.add_space(8.0);
            inner.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 12.0;
                let button = |ui: &mut egui::Ui, icon, label| {
                    widgets::button(ui, Some(icon), label, Kind::Secondary).clicked()
                };
                if button(ui, icons::SKIP_FORWARD, tr!("jf-play-next")) {
                    go = Some(Go::Queue(item.clone(), true));
                }
                if button(ui, icons::QUEUE, tr!("jf-add-to-queue")) {
                    go = Some(Go::Queue(item.clone(), false));
                }
                if button(ui, icons::PLAYLIST, tr!("jf-add-to-playlist")) {
                    go = Some(Go::AddToPlaylist(item.clone()));
                }
            });
        }
        inner.add_space(8.0);
        inner.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 8.0;
            go = go.take().or(mark_chips(ui, item, caps));
            if item.kind == CatalogKind::Episode
                && let Some(id) = &item.series_id
                && widgets::chip_icon(ui, Some(icons::MONITOR_PLAY), tr!("jf-series"), false)
                    .clicked()
            {
                go = Some(Go::Open(CatalogItem {
                    id: id.clone(),
                    name: item.series_name.clone().unwrap_or_default(),
                    kind: CatalogKind::Series,
                    ..CatalogItem::default()
                }));
            }
        });
    });
    ui.add_space(20.0);
    ui.set_max_width(1100.0);
    if item.versions.len() > 1 {
        if let Some(version) = version_chips(ui, item) {
            item.version = version.clone();
            go = Some(Go::Version(item.location.clone(), version));
        }
        ui.add_space(14.0);
    }
    if let Some(o) = &item.overview {
        page::prose(ui, o);
        ui.add_space(14.0);
    }
    let line = |ui: &mut egui::Ui, label: &str, text: String| {
        if text.is_empty() {
            return;
        }
        widgets::section_label(ui, label);
        ui.label(RichText::new(text).color(theme::TEXT_2));
        ui.add_space(10.0);
    };
    line(ui, tr!("filter-genres"), item.genres.join("  ·  "));
    let (cast, crew): (Vec<_>, Vec<_>) = item
        .people
        .iter()
        .partition(|(_, role)| !matches!(role.as_str(), "Director" | "Writer" | "Producer"));
    line(
        ui,
        tr!("jf-cast"),
        cast.iter()
            .take(12)
            .map(|(n, r)| {
                if r.is_empty() || r == "Actor" {
                    n.clone()
                } else {
                    format!("{n} ({r})")
                }
            })
            .collect::<Vec<_>>()
            .join(",  "),
    );
    line(
        ui,
        tr!("jf-crew"),
        crew.iter()
            .map(|(n, r)| format!("{n} ({r})"))
            .collect::<Vec<_>>()
            .join(",  "),
    );
    line(ui, tr!("filter-studios"), item.studios.join("  ·  "));
    if caps.tags && !item.tags.is_empty() {
        widgets::section_label(ui, tr!("filter-tags"));
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = Vec2::new(8.0, 8.0);
            for t in &item.tags {
                if widgets::chip(ui, t, false).clicked() {
                    go = Some(Go::Tag(t.clone()));
                }
            }
        });
    }
    ui.add_space(20.0);
    go
}

/// The versions the server keeps of a film or video (4K and 1080p, a
/// director's cut); the one clicked, to play it from wherever it is opened.
fn version_chips(ui: &mut egui::Ui, item: &CatalogItem) -> Option<Option<String>> {
    let mut chosen = None;
    widgets::section_label(ui, tr!("jf-version"));
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = Vec2::new(8.0, 8.0);
        if widgets::chip(ui, tr!("jf-version-automatic"), item.version.is_none())
            .tip(tr!("jf-version-automatic-tip"))
            .clicked()
        {
            chosen = Some(None);
        }
        for version in &item.versions {
            let on = item.version.as_deref() == Some(version.id.as_str());
            let label = match format_badge(version.format) {
                Some(b) => format!("{} · {b}", version.label()),
                None => version.label(),
            };
            if widgets::chip(ui, &label, on).clicked() {
                chosen = Some(Some(version.id.clone()));
            }
        }
    });
    chosen
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(id: &str, kind: CatalogKind) -> CatalogItem {
        CatalogItem {
            id: id.into(),
            name: id.into(),
            kind,
            ..CatalogItem::default()
        }
    }

    #[test]
    fn answers_reach_the_page_that_asked() {
        let mut j = CatalogView::new(1, None);
        j.pages.push(Page {
            seq: 2,
            request_seq: None,
            loading: true,
            error: None,
            loaded_at: None,
            body: Body::Grid(Box::new(Grid::new(
                "Movies".into(),
                CatalogQuery {
                    parent: Some("lib".into()),
                    ..CatalogQuery::default()
                },
            ))),
        });
        let page = |n: usize| CatalogPage {
            items: (0..n)
                .map(|i| item(&i.to_string(), CatalogKind::Movie))
                .collect(),
            total: 100,
        };
        let grid = |page: CatalogPage, append: bool| Loaded::Grid {
            page,
            append,
            order: None,
            by_id: false,
        };
        j.loaded(2, Ok(grid(page(60), false)));
        j.loaded(2, Ok(grid(page(40), true)));
        // An answer for a page that is gone is dropped.
        j.loaded(9, Ok(grid(page(5), false)));
        let Body::Grid(g) = &j.pages[1].body else {
            panic!()
        };
        assert_eq!((g.items.len(), g.total), (100, 100));
        assert!(!j.pages[1].loading);
        // The home page is still waiting.
        assert!(j.pages[0].loading);
        j.loaded(1, Err("offline".into()));
        assert_eq!(j.pages[0].error.as_deref(), Some("offline"));

        // The header says where the page is.
        assert!(trail(&j.pages[..1]).is_empty());
        assert_eq!(trail(&j.pages), "Movies");

        // A reload covers everything shown.
        let (seq, load) = j.reload().unwrap();
        assert_eq!(seq, 2);
        let Load::Grid(q) = load else { panic!() };
        assert_eq!((q.start, q.limit), (0, 100));
    }

    #[test]
    fn late_unfiltered_answers_cannot_replace_a_studio_filtered_listing() {
        let mut view = CatalogView::new(1, None);
        let query = CatalogQuery::default();
        view.pages[0].body = Body::Grid(Box::new(Grid::new("Movies".into(), query.clone())));
        let old = view.begin_request(1, 2, &Load::Grid(query)).unwrap();
        let Body::Grid(grid) = &mut view.pages[0].body else {
            panic!()
        };
        grid.query.studios = vec!["Selected studio".into()];
        let query = grid.restart();
        let current = view
            .begin_request(1, 3, &Load::Grid(query.clone()))
            .unwrap();
        let facets = view.begin_request(1, 4, &Load::Facets(query)).unwrap();
        assert_eq!(facets, 1, "facets retain the page's independent identity");

        view.loaded(old, Err("an obsolete request failed".into()));
        assert!(view.pages[0].loading);
        assert!(view.pages[0].error.is_none());
        view.loaded(
            facets,
            Ok(Loaded::Facets(Ok(CatalogFacets {
                studios: vec!["Selected studio".into()],
                ..Default::default()
            }))),
        );
        assert!(view.pages[0].loading, "facets do not finish the listing");
        let reply = |id: &str| Loaded::Grid {
            page: CatalogPage {
                items: vec![item(id, CatalogKind::Movie)],
                total: 1,
            },
            append: false,
            order: None,
            by_id: false,
        };
        view.loaded(current, Ok(reply("filtered")));
        view.loaded(old, Ok(reply("unfiltered")));
        view.loaded(old, Err("another stale failure".into()));
        let page = &view.pages[0];
        assert_eq!(page.seq, 1);
        assert!(!page.loading);
        assert!(page.error.is_none());
        let Body::Grid(grid) = &page.body else {
            panic!()
        };
        assert_eq!(grid.items[0].id, "filtered");
        assert_eq!(grid.query.studios, ["Selected studio"]);
        assert!(matches!(&grid.facets, Facets::Got(f) if f.studios == ["Selected studio"]));

        view.loaded(current, Err("the current listing failed".into()));
        view.loaded(facets, Ok(Loaded::Facets(Ok(CatalogFacets::default()))));
        assert_eq!(
            view.pages[0].error.as_deref(),
            Some("the current listing failed")
        );
    }

    #[test]
    fn changing_filters_discards_old_shuffle_ids_and_in_flight_pages() {
        let mut view = CatalogView::new(1, None);
        let mut grid = Grid::new(
            "Movies".into(),
            CatalogQuery {
                sort: CatalogSort::Random,
                ..Default::default()
            },
        );
        grid.items = vec![item("old-first", CatalogKind::Movie)];
        grid.order = Some(vec!["old-first".into(), "old-next".into()]);
        grid.total = 2;
        let next = grid.next_page();
        assert_eq!(next.ids, ["old-next"]);
        view.pages[0].body = Body::Grid(Box::new(grid));
        let old_page = view.begin_request(1, 2, &Load::Grid(next)).unwrap();
        let Body::Grid(grid) = &mut view.pages[0].body else {
            panic!()
        };
        grid.query.studios = vec!["New studio".into()];
        let changed = grid.restart();
        assert!(changed.ids.is_empty());
        assert!(grid.order.is_none());
        let first_filter = view.begin_request(1, 3, &Load::Grid(changed)).unwrap();

        // Refreshing before the filtered response arrives must not reuse
        // IDs from the old shuffle, even though its cards are still visible.
        let (page, reload) = view.reload().unwrap();
        let Load::Grid(query) = &reload else { panic!() };
        assert_eq!(query.studios, ["New studio"]);
        assert!(query.ids.is_empty());
        let refreshed = view.begin_request(page, 4, &reload).unwrap();
        let reply = |id: &str, append: bool| Loaded::Grid {
            page: CatalogPage {
                items: vec![item(id, CatalogKind::Movie)],
                total: 1,
            },
            append,
            order: (!append).then(|| vec![id.into()]),
            by_id: true,
        };
        view.loaded(old_page, Ok(reply("old-next", true)));
        view.loaded(first_filter, Ok(reply("earlier-filter", false)));
        assert!(view.pages[0].loading);
        view.loaded(refreshed, Ok(reply("new-studio", false)));
        view.loaded(old_page, Ok(reply("old-next", true)));
        let Body::Grid(grid) = &view.pages[0].body else {
            panic!()
        };
        assert_eq!(grid.items.len(), 1);
        assert_eq!(grid.items[0].id, "new-studio");
        assert_eq!(grid.order.as_deref().unwrap(), ["new-studio"]);
    }

    #[test]
    fn a_reload_that_supersedes_a_facet_action_does_not_strand_the_drawer() {
        let mut view = CatalogView::new(1, None);
        let mut grid = Grid::new("Movies".into(), CatalogQuery::default());
        grid.filters_open = true;
        let first_facets = grid.unasked_facets().unwrap();

        // The drawer offers a facets action, but the search debounce
        // expires in the same frame and its reload takes precedence.
        grid.query.search = "film".into();
        let query = grid.restart();
        view.pages[0].body = Body::Grid(Box::new(grid));
        view.begin_request(1, 2, &Load::Grid(query)).unwrap();
        let Body::Grid(grid) = &view.pages[0].body else {
            panic!()
        };
        assert!(grid.unasked_facets().is_some());
        assert!(matches!(grid.facets, Facets::Unasked));

        // On the next frame a dispatched facets request is marked Asked
        // and can finish independently of the pending search response.
        let seq = view
            .begin_request(1, 3, &Load::Facets(first_facets))
            .unwrap();
        let Body::Grid(grid) = &view.pages[0].body else {
            panic!()
        };
        assert!(grid.unasked_facets().is_none());
        assert!(matches!(grid.facets, Facets::Asked));
        view.loaded(seq, Ok(Loaded::Facets(Ok(CatalogFacets::default()))));
        assert!(view.pages[0].loading);
        assert!(
            matches!(&view.pages[0].body, Body::Grid(grid) if matches!(grid.facets, Facets::Got(_)))
        );
    }

    #[test]
    fn a_shuffle_pages_through_its_own_order() {
        let mut j = CatalogView::new(1, None);
        let mut g = Grid::new("Movies".into(), CatalogQuery::default());
        g.query.sort = CatalogSort::Random;
        j.pages[0].body = Body::Grid(Box::new(g));
        let order: Vec<String> = (0..150).map(|i| format!("id{i}")).collect();
        let items = |ids: &[String]| CatalogPage {
            items: ids.iter().map(|i| item(i, CatalogKind::Movie)).collect(),
            total: 0,
        };
        j.loaded(
            1,
            Ok(Loaded::Grid {
                page: items(&order[..60]),
                append: false,
                order: Some(order.clone()),
                by_id: true,
            }),
        );
        let Body::Grid(g) = &j.pages[0].body else {
            panic!()
        };
        assert_eq!(g.total, 150, "the shuffle's length");
        // The next page is the order's next 60, by id.
        let next = g.next_page();
        assert_eq!((next.start, next.ids.len()), (60, 60));
        assert_eq!(next.ids[0], "id60");
        j.loaded(
            1,
            Ok(Loaded::Grid {
                page: items(&next.ids),
                append: true,
                order: None,
                by_id: true,
            }),
        );
        // A reload (after watching something) keeps the order.
        let Some((_, Load::Grid(q))) = j.reload() else {
            panic!()
        };
        assert_eq!(q.ids.len(), 120);
        assert_eq!(q.ids[..3], order[..3]);
        j.loaded(
            1,
            Ok(Loaded::Grid {
                page: items(&q.ids),
                append: false,
                order: None,
                by_id: true,
            }),
        );
        let Body::Grid(g) = &j.pages[0].body else {
            panic!()
        };
        assert_eq!((g.items.len(), g.total), (120, 150));
        assert!(g.order.is_some());
        // The drawer's choices don't end the page's loading.
        j.pages[0].loading = true;
        j.loaded(1, Ok(Loaded::Facets(Ok(CatalogFacets::default()))));
        assert!(j.pages[0].loading);
    }

    #[test]
    fn filters_are_chips_until_taken_off() {
        let mut g = Grid::new("Tagged sea".into(), CatalogQuery::tagged("sea"));
        assert!(g.labels().is_empty(), "the page's own tag is no filter");
        g.query.genres = vec!["Drama".into()];
        g.query.years = (1990..2000).chain([2004]).collect();
        g.query.min_width = Some(3840);
        g.query.tags.push("night".into());
        assert_eq!(
            g.labels(),
            ["Genre · Drama", "1990s", "4K and up", "Tag · night"],
            "a decade counts whole"
        );
        g.take_off(1);
        assert_eq!(g.query.years, [2004]);
        g.clear_filters();
        assert!(g.labels().is_empty());
        assert_eq!(g.query.tags, ["sea"]);
        assert_eq!(decades(&[2021, 1999, 2020, 1985]), [2020, 1990, 1980]);
    }

    #[test]
    fn marks_show_everywhere() {
        let mut j = CatalogView::new(1, None);
        let mut watched = item("e1", CatalogKind::Episode);
        watched.position = 300.0;
        j.loaded(
            1,
            Ok(Loaded::Home(CatalogHome {
                resume: vec![watched.clone()],
                ..CatalogHome::default()
            })),
        );
        j.pages.push(Page {
            seq: 2,
            request_seq: None,
            loading: false,
            error: None,
            loaded_at: None,
            body: Body::Item {
                item: Box::new(watched),
            },
        });
        j.mark("e1", Mark::Played, true);
        j.mark("e1", Mark::Favorite, true);
        let Body::Item { item } = &j.pages[1].body else {
            panic!()
        };
        assert!(item.played && item.favorite && item.position == 0.0);
        let Body::Home { home: Some(h), .. } = &j.pages[0].body else {
            panic!()
        };
        assert!(h.resume[0].played);
    }

    #[test]
    fn search_pages_show_rows_by_kind() {
        let mut j = CatalogView::new(1, None);
        j.pages.push(Page {
            seq: 2,
            request_seq: None,
            loading: true,
            error: None,
            loaded_at: None,
            body: Body::Search {
                search: "alien".into(),
                typed: None,
                asked: "alien".into(),
                results: vec![],
            },
        });
        assert!(!has_content(&j.pages[1].body));
        j.loaded(
            2,
            Ok(Loaded::Search {
                text: "aliens".into(),
                results: vec![
                    ("Movies", vec![item("m1", CatalogKind::Movie)]),
                    ("Shows", vec![item("s1", CatalogKind::Series)]),
                ],
            }),
        );
        let Body::Search { asked, results, .. } = &j.pages[1].body else {
            panic!()
        };
        assert_eq!(asked, "aliens");
        assert_eq!(results.len(), 2);
        assert_eq!(trail(&j.pages), "Search \"aliens\"");

        // Marks show in the results; a reload searches again.
        j.mark("s1", Mark::Favorite, true);
        let Body::Search { results, .. } = &j.pages[1].body else {
            panic!()
        };
        assert!(results[1].1[0].favorite);
        let (seq, load) = j.reload().unwrap();
        assert_eq!(seq, 2);
        assert!(matches!(load, Load::Search(t) if t == "aliens"));
    }

    fn row(id: &str, title: &str, kind: CatalogKind) -> CatalogRow {
        let items = vec![item(&format!("{id}-1"), kind)];
        CatalogRow {
            id: id.into(),
            title: title.into(),
            shape: RowShape::of(&items),
            items,
            ..CatalogRow::default()
        }
    }

    fn home_with(rows: Vec<CatalogRow>) -> CatalogHome {
        let library = item("lib", CatalogKind::Library);
        CatalogHome {
            libraries: vec![library.clone()],
            resume: vec![item("e1", CatalogKind::Episode)],
            next_up: vec![item("n1", CatalogKind::Episode)],
            latest: vec![(library, vec![item("m1", CatalogKind::Movie)])],
            rows,
        }
    }

    fn shelf_ids(h: &CatalogHome) -> Vec<String> {
        home_shelves(h).into_iter().map(|s| s.id).collect()
    }

    #[test]
    fn server_rows_replace_latest_and_keep_their_order() {
        assert_eq!(
            shelf_ids(&home_with(Vec::new())),
            ["my-media", "continue", "next-up", "latest:lib"],
            "a server without rows keeps the old front page"
        );
        let h = home_with(vec![
            row("b", "Second in the list", CatalogKind::Movie),
            row("a", "First in the alphabet", CatalogKind::Episode),
        ]);
        assert_eq!(shelf_ids(&h), ["my-media", "continue", "row:b", "row:a"]);
        let shelves = home_shelves(&h);
        assert_eq!(shelves[2].title, "Second in the list");
        assert_eq!(shelves[2].shape, Some(RowShape::Portrait));
        assert!(shelves[2].portrait(0));
        assert_eq!(shelves[3].shape, Some(RowShape::Landscape));
        assert!(!shelves[3].portrait(0));
    }

    #[test]
    fn marks_reach_server_rows() {
        let mut j = CatalogView::new(1, None);
        let h = home_with(vec![row("a", "Row", CatalogKind::Movie)]);
        j.loaded(1, Ok(Loaded::Home(h)));
        j.mark("a-1", Mark::Played, true);
        j.mark("a-1", Mark::Favorite, true);
        let Body::Home { home: Some(h), .. } = &j.pages[0].body else {
            panic!()
        };
        assert!(h.rows[0].items[0].played && h.rows[0].items[0].favorite);
        assert!(!h.resume[0].played);
    }

    #[test]
    fn a_home_row_card_plays_alone() {
        let list: Vec<CatalogItem> = ["m1", "m2", "m3"]
            .iter()
            .map(|id| item(id, CatalogKind::Movie))
            .collect();
        let click = page::CardResponse {
            open: true,
            ..page::CardResponse::default()
        };
        let Some(Go::Play(alone, at, false)) = card_go(click, &list, 1, true) else {
            panic!()
        };
        assert_eq!((alone.len(), at, alone[0].id.as_str()), (1, 0, "m2"));
        let Some(Go::Play(queued, at, false)) = card_go(click, &list, 1, false) else {
            panic!()
        };
        assert_eq!((queued.len(), at), (3, 1));
        // A folder opens whichever way, a click elsewhere on the card is no play.
        let folder = [item("c1", CatalogKind::BoxSet)];
        assert!(matches!(
            card_go(click, &folder, 0, true),
            Some(Go::Open(_))
        ));
        assert!(card_go(page::CardResponse::default(), &list, 0, true).is_none());

        let h = home_with(vec![row("a", "Row", CatalogKind::Movie)]);
        let alone: Vec<bool> = home_shelves(&h).iter().map(|s| s.alone).collect();
        assert_eq!(alone, [false, true, true]);
        let old: Vec<bool> = home_shelves(&home_with(Vec::new()))
            .iter()
            .map(|s| s.alone)
            .collect();
        assert_eq!(old, [false, true, false, false]);
    }

    #[test]
    fn see_all_opens_the_rows_listing() {
        let more = CatalogQuery {
            hub: Some("/hubs/made-up/rows".into()),
            sort: CatalogSort::Server,
            limit: 12,
            start: 12,
            ..CatalogQuery::default()
        };
        let mut a = row("a", "Row title", CatalogKind::Movie);
        a.more = Some(more.clone());
        let b = row("b", "Complete row", CatalogKind::Movie);
        let h = home_with(vec![a, b]);
        let shelves = home_shelves(&h);
        assert_eq!(shelves[2].more, Some(&more));
        assert!(shelves[3].more.is_none(), "a complete row has no See all");

        let (load, body) = see_all_page("Row title".into(), more.clone());
        let Load::Grid(q) = load else { panic!() };
        assert_eq!(q.hub, more.hub);
        assert_eq!((q.start, q.limit, q.sort), (0, PAGE, CatalogSort::Server));
        let Body::Grid(g) = body else { panic!() };
        assert_eq!(g.title, "Row title");
        assert!(g.server_order);
        assert_eq!(g.query, q);
    }

    fn plex_caps() -> CatalogCapabilities {
        CatalogCapabilities {
            library_pages: true,
            ..CatalogCapabilities::default()
        }
    }

    /// A page open on a library of a server with library pages.
    fn library_view() -> CatalogView {
        let mut lib = item("section:1", CatalogKind::Library);
        lib.collection_type = Some("movies".into());
        let (_, body) = page_for(lib, plex_caps());
        let mut j = CatalogView::new(1, None);
        j.pages[0].body = body;
        j
    }

    fn library_of(j: &mut CatalogView) -> &mut LibraryPage {
        let Body::Library(l) = &mut j.pages[0].body else {
            panic!("not a library page")
        };
        l
    }

    /// Asks as the page does and has the server answer.
    fn ask(j: &mut CatalogView, request: u64, load: &Load, answer: Loaded) {
        j.begin_request(1, request, load).unwrap();
        j.loaded(request, Ok(answer));
    }

    fn page_of(items: Vec<CatalogItem>) -> Loaded {
        Loaded::Grid {
            page: CatalogPage {
                total: items.len(),
                items,
            },
            append: false,
            order: None,
            by_id: false,
        }
    }

    fn rows_load() -> Load {
        Load::LibraryRows("section:1".into())
    }

    fn some_rows() -> Loaded {
        Loaded::LibraryRows(vec![row("a", "Recently Added", CatalogKind::Movie)])
    }

    #[test]
    fn a_plex_library_opens_on_its_recommended_rows() {
        let mut lib = item("section:1", CatalogKind::Library);
        lib.collection_type = Some("movies".into());
        let (load, body) = page_for(lib, plex_caps());
        assert!(matches!(&load, Load::LibraryRows(id) if id == "section:1"));
        let Body::Library(l) = body else {
            panic!("not a library page")
        };
        assert_eq!(l.tab, LibraryTab::Recommended);
        assert!(l.rows.is_none());
        assert_eq!(l.grid.query.parent.as_deref(), Some("section:1"));
        assert_eq!(l.grid.query.types, [CatalogKind::Movie]);
        assert_eq!(l.collections.query.parent.as_deref(), Some("section:1"));
        assert_eq!(l.collections.query.types, [CatalogKind::BoxSet]);
        assert!(l.next_load(false).is_none(), "the rows are already asked");
    }

    #[test]
    fn an_empty_recommended_shows_the_library() {
        let mut j = library_view();
        ask(&mut j, 2, &rows_load(), Loaded::LibraryRows(Vec::new()));
        let l = library_of(&mut j);
        assert_eq!(l.tab, LibraryTab::Library);
        assert!(l.tabs().iter().all(|t| t.0 != LibraryTab::Recommended));
        assert!(l.next_load(true).is_none(), "not while something loads");
        let Some(Load::Grid(q)) = l.next_load(false) else {
            panic!("the titles are asked for now")
        };
        assert_eq!((q.start, q.types), (0, vec![CatalogKind::Movie]));
    }

    #[test]
    fn a_failed_recommended_shows_the_library_and_the_tab_asks_again() {
        let mut j = library_view();
        j.begin_request(1, 2, &rows_load());
        j.loaded(2, Err("the server is asleep".into()));
        assert_eq!(j.pages[0].error.as_deref(), Some("the server is asleep"));
        let l = library_of(&mut j);
        assert_eq!(l.tab, LibraryTab::Library);
        assert!(l.tabs().iter().any(|t| t.0 == LibraryTab::Recommended));
        l.show(LibraryTab::Recommended);
        assert!(matches!(l.next_load(false), Some(Load::LibraryRows(_))));
    }

    #[test]
    fn tabs_load_when_first_shown() {
        let mut j = library_view();
        ask(&mut j, 2, &rows_load(), some_rows());
        let l = library_of(&mut j);
        assert_eq!(l.tab, LibraryTab::Recommended);
        assert_eq!(l.tabs().len(), 3);
        assert!(l.next_load(false).is_none(), "Recommended is there");

        // Collections, shown for the first time, asks for the collections.
        l.show(LibraryTab::Collections);
        let Some(load) = l.next_load(false) else {
            panic!("the collections are asked for now")
        };
        assert_eq!(load.tab(), LibraryTab::Collections);
        let Load::Grid(q) = &load else {
            panic!("a listing is asked for")
        };
        assert_eq!(q.types, [CatalogKind::BoxSet]);
        assert_eq!(q.parent.as_deref(), Some("section:1"));
        j.begin_request(1, 3, &load).unwrap();
        assert!(library_of(&mut j).next_load(false).is_none());

        // The user goes on to the titles while the collections come; the
        // answer still lands on the collections.
        library_of(&mut j).show(LibraryTab::Library);
        assert!(library_of(&mut j).next_load(true).is_none());
        let collection = item("collection:9", CatalogKind::BoxSet);
        j.loaded(3, Ok(page_of(vec![collection])));
        let l = library_of(&mut j);
        assert_eq!((l.collections.items.len(), l.grid.items.len()), (1, 0));
        assert_eq!(l.collections.total, 1);

        // Now the titles are asked for, once.
        let Some(load) = l.next_load(false) else {
            panic!("the titles are asked for now")
        };
        assert_eq!(load.tab(), LibraryTab::Library);
        j.begin_request(1, 4, &load).unwrap();
        j.loaded(4, Ok(page_of(vec![item("m1", CatalogKind::Movie)])));
        let l = library_of(&mut j);
        assert_eq!((l.collections.items.len(), l.grid.items.len()), (1, 1));
        assert!(l.next_load(false).is_none());
        l.show(LibraryTab::Collections);
        assert!(l.next_load(false).is_none(), "a tab that came stays");
    }

    #[test]
    fn a_library_page_reloads_the_tab_shown_and_the_others_when_shown() {
        let mut j = library_view();
        ask(&mut j, 2, &rows_load(), some_rows());
        library_of(&mut j).show(LibraryTab::Library);
        let load = library_of(&mut j).next_load(false).unwrap();
        let titles = page_of(vec![item("m1", CatalogKind::Movie)]);
        ask(&mut j, 3, &load, titles);

        let (seq, load) = j.reload().unwrap();
        assert_eq!(seq, 1);
        let Load::Grid(q) = load else {
            panic!("the titles show again")
        };
        assert_eq!(q.types, [CatalogKind::Movie]);
        let l = library_of(&mut j);
        assert!(l.asked[LibraryTab::Library.index()]);
        l.show(LibraryTab::Recommended);
        assert!(matches!(l.next_load(false), Some(Load::LibraryRows(_))));
    }

    #[test]
    fn marks_reach_every_tab_of_a_library() {
        let mut j = library_view();
        ask(&mut j, 2, &rows_load(), some_rows());
        library_of(&mut j).show(LibraryTab::Library);
        let load = library_of(&mut j).next_load(false).unwrap();
        let titles = page_of(vec![item("a-1", CatalogKind::Movie)]);
        ask(&mut j, 3, &load, titles);
        j.mark("a-1", Mark::Played, true);
        let l = library_of(&mut j);
        assert!(l.grid.items[0].played);
        assert!(l.rows.as_ref().unwrap()[0].items[0].played);
    }

    #[test]
    fn the_filters_drawer_belongs_to_the_listing_shown() {
        let mut j = library_view();
        j.show_filters(true);
        assert!(!j.filters_open(), "Recommended has no filters");
        assert!(j.show_tab("library"));
        j.show_filters(true);
        assert!(j.filters_open());
        assert!(j.show_tab("collections"));
        assert!(!j.filters_open());
        assert!(!j.show_tab("nonsense"));
    }

    #[test]
    fn a_jellyfin_library_still_opens_as_a_grid() {
        let mut lib = item("lib", CatalogKind::Library);
        lib.collection_type = Some("movies".into());
        let (load, body) = page_for(lib, CatalogCapabilities::default());
        let Load::Grid(q) = load else {
            panic!("a grid is asked for")
        };
        assert_eq!(q.parent.as_deref(), Some("lib"));
        assert!(matches!(body, Body::Grid(_)));
    }

    #[test]
    fn a_collection_opens_in_its_own_order() {
        let collection = item("collection:9", CatalogKind::BoxSet);
        let (_, body) = page_for(collection, plex_caps());
        let Body::Grid(g) = body else {
            panic!("a collection is a grid")
        };
        assert!(g.server_order);
        assert_eq!(g.query.sort, CatalogSort::Server);
    }

    #[test]
    fn a_cached_home_shows_while_it_reloads() {
        let first = home_with(vec![row("a", "Old row", CatalogKind::Movie)]);
        let mut cache = HomeCache::default();
        assert!(cache.get("plex").is_none());
        cache.put("plex", &first);
        assert!(cache.get("other").is_none());

        let mut j = CatalogView::new(1, cache.get("plex"));
        assert!(j.pages[0].loading, "the fresh copy is on its way");
        assert!(has_content(&j.pages[0].body), "and the old one is drawn");
        j.loaded(1, Ok(Loaded::Home(home_with(Vec::new()))));
        let Body::Home { home: Some(h), .. } = &j.pages[0].body else {
            panic!("the front page")
        };
        assert!(h.rows.is_empty(), "the answer replaces it");

        let later = Instant::now() + HOME_LIFE + Duration::from_secs(1);
        assert!(cache.get_at("plex", later).is_none(), "long out of date");
    }

    #[test]
    fn showing_the_browser_again_soon_does_not_refetch() {
        let mut j = CatalogView::new(1, None);
        assert!(!j.loaded_recently(), "nothing has answered yet");
        j.loaded(1, Err("asleep".into()));
        assert!(!j.loaded_recently(), "an error is no answer");
        j.loaded(1, Ok(Loaded::Home(home_with(Vec::new()))));
        assert!(j.loaded_recently());
        let later = Instant::now() + FRESH + Duration::from_secs(1);
        assert!(!j.loaded_recently_at(later));
    }

    #[test]
    fn the_servers_order_is_offered_first_where_the_listing_began_in_it() {
        assert_eq!(sorts(true)[0].0, CatalogSort::Server);
        assert_eq!(sorts(true).len(), sorts(false).len() + 1);
        assert!(sorts(false).iter().all(|s| s.0 != CatalogSort::Server));
        let collection = CatalogQuery::children(&item("c1", CatalogKind::BoxSet));
        assert!(Grid::new("Collection".into(), collection).server_order);
        assert!(!Grid::new("Movies".into(), CatalogQuery::default()).server_order);
    }

    #[test]
    fn a_playlist_is_a_landscape_card_with_its_item_count() {
        let mut list = item("p1", CatalogKind::Playlist);
        list.child_count = Some(3);
        assert_eq!(card_facts(&list), "3 items");
        assert!(!list.kind.portrait() && !list.kind.playable());
    }

    #[test]
    fn requests_resume_from_the_server() {
        let mut m = item("m1", CatalogKind::Movie);
        m.location = "mb:http://jf#item=m1".into();
        assert_eq!(request("jf", &m, false).start_at, None);
        m.position = 120.0;
        assert_eq!(request("jf", &m, false).start_at, Some(120.0));
        assert_eq!(request("jf", &m, true).start_at, Some(0.0));
        let r = request("jf", &m, false);
        assert_eq!(r.source_id.as_deref(), Some("jf"));
        assert_eq!(r.entry.unwrap().location, "mb:http://jf#item=m1");
        assert_eq!(clip("one two three four", 9), "one two…");
        assert_eq!(clip("short", 9), "short");
    }
}
