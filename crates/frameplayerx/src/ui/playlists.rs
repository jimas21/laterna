//! Playlists: the tab listing them, a playlist's own page, and the picker
//! that adds videos to one.

use super::page::{self, Card};
use super::theme::{self, Weight};
use super::widgets::{self, Kind};
use super::{Action, Screen, View, icons};
use egui::RichText;
use fp_library::{MediaRecord, Playlist, PlaylistId};
use fp_sources::CatalogItem;

/// How many videos a server's watch history lists.
pub const HISTORY_LIMIT: usize = 60;

/// What a server answered to a list a job asked it for.
#[derive(Clone, Debug)]
pub enum Asked {
    Loading,
    Got(Vec<CatalogItem>),
    Failed(String),
}

/// One server's video playlists on the Playlists tab.
#[derive(Clone, Debug)]
pub struct ServerPlaylists {
    pub source: String,
    pub name: String,
    pub lists: Asked,
}

/// What a server's watch history lists on the History screen.
#[derive(Clone, Debug)]
pub struct ServerHistory {
    pub source: String,
    pub items: Asked,
}

/// The servers that keep playlists and watch history of their own, as their
/// id and name: the Plex sources that are not locked.
pub fn servers(v: &View) -> Vec<(String, String)> {
    v.services
        .source_configs
        .iter()
        .filter(|c| c.kind() == fp_sources::SourceKind::Plex)
        .filter(|c| !crate::pin::locked(&v.settings.source_pins, &v.state.unlocked_sources, c.id()))
        .map(|c| (c.id().to_string(), c.name().to_string()))
        .collect()
}

/// The Playlists tab: your playlists, those of each server, or the one open.
pub fn page(ui: &mut egui::Ui, v: &mut View) {
    if let Some(id) = v.state.playlist {
        playlist(ui, v, id);
        return;
    }
    ui.add_space(12.0);
    ui.label(
        RichText::new(tr!("nav-playlists"))
            .font(theme::font(Weight::Bold, 30.0))
            .color(theme::TEXT),
    );
    ui.add_space(10.0);
    new_playlist(ui, v, &[]);
    ui.add_space(14.0);
    let lists = playlists(v);
    let sections = server_sections(v);
    // A server that is still being asked, or could not answer, has a section;
    // one with no playlists has none.
    let any_server = sections.iter().any(|s| match &s.lists {
        Asked::Got(found) => !found.is_empty(),
        _ => true,
    });
    if lists.is_empty() && !any_server {
        page::empty_state(
            ui,
            icons::PLAYLIST,
            tr!("playlists-none"),
            tr!("playlists-none-desc"),
            |_| {},
        );
        v.state.server_playlists.get_or_insert(sections);
        return;
    }
    let mut picked = None;
    let mut opened = None;
    page::scroll(ui, |ui| {
        if !lists.is_empty() {
            picked = grid_of(ui, v, &lists);
        }
        for section in &sections {
            if matches!(&section.lists, Asked::Got(found) if found.is_empty()) {
                continue;
            }
            widgets::section_label(
                ui,
                &tr!("playlists-on-server", server = section.name.as_str()),
            );
            ui.add_space(6.0);
            match &section.lists {
                Asked::Loading => {
                    ui.label(
                        RichText::new(tr!("common-loading"))
                            .font(theme::font(Weight::Regular, 16.0))
                            .color(theme::TEXT_2),
                    );
                    ui.add_space(14.0);
                }
                Asked::Failed(error) => {
                    ui.label(
                        RichText::new(tr!("playlists-server-failed", error = error.as_str()))
                            .font(theme::font(Weight::Regular, 16.0))
                            .color(theme::TEXT_2),
                    );
                    ui.add_space(14.0);
                }
                Asked::Got(found) => {
                    page::grid(ui, found.len(), page::LANDSCAPE.x, |ui, i| {
                        let c = super::catalog::item_card(ui, v, &found[i], false);
                        if c.open {
                            opened = Some((section.source.clone(), found[i].clone()));
                        }
                    });
                }
            }
        }
    });
    // The answers that arrived while the page was drawn are not lost.
    v.state.server_playlists.get_or_insert(sections);
    if let Some(id) = picked {
        v.state.playlist = Some(id);
        v.state.renaming = false;
    }
    if let Some((source, item)) = opened {
        v.actions.push(Action::OpenSourceAt {
            source,
            item: Box::new(item),
            back_to: Screen::Playlists,
        });
    }
}

/// Each server's playlists as last asked for; the servers are asked, once,
/// the first time the tab shows after a change to the library. The state is
/// taken: the caller puts it back.
fn server_sections(v: &mut View) -> Vec<ServerPlaylists> {
    if let Some(sections) = v.state.server_playlists.take() {
        return sections;
    }
    let sections: Vec<ServerPlaylists> = servers(v)
        .into_iter()
        .map(|(source, name)| ServerPlaylists {
            source,
            name,
            lists: Asked::Loading,
        })
        .collect();
    for section in &sections {
        v.actions.push(Action::ServerPlaylists {
            source: section.source.clone(),
        });
    }
    sections
}

/// The History screen's choice between this headset and each server.
pub fn history_chips(ui: &mut egui::Ui, v: &mut View, servers: &[(String, String)]) {
    ui.add_space(8.0);
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 10.0;
        let on_headset = v.state.history_source.is_none();
        if widgets::chip(ui, tr!("history-this-headset"), on_headset).clicked() {
            v.state.history_source = None;
        }
        for (id, name) in servers {
            let on = v.state.history_source.as_deref() == Some(id.as_str());
            if widgets::chip(ui, name, on).clicked() && !on {
                v.state.history_source = Some(id.clone());
                v.state.server_history = None;
            }
        }
    });
    ui.add_space(8.0);
}

/// What the server chosen on the History screen watched, newest first; a card
/// plays that video alone, from where it was left.
pub fn server_history(ui: &mut egui::Ui, v: &mut View) {
    let Some(source) = v.state.history_source.clone() else {
        return;
    };
    let history = match v.state.server_history.take() {
        Some(h) if h.source == source => h,
        _ => {
            v.actions.push(Action::ServerHistory {
                source: source.clone(),
            });
            ServerHistory {
                source: source.clone(),
                items: Asked::Loading,
            }
        }
    };
    let mut play = None;
    let mut more = None;
    match &history.items {
        Asked::Loading => {
            ui.add_space(24.0);
            ui.label(
                RichText::new(tr!("common-loading"))
                    .font(theme::font(Weight::Regular, 18.0))
                    .color(theme::TEXT_2),
            );
        }
        Asked::Failed(error) => page::empty_state(
            ui,
            icons::CLOCK,
            tr!("history-server-failed"),
            error,
            |_| {},
        ),
        Asked::Got(items) if items.is_empty() => page::empty_state(
            ui,
            icons::CLOCK,
            tr!("library-nothing-watched"),
            tr!("history-server-empty-desc"),
            |_| {},
        ),
        Asked::Got(items) => page::scroll(ui, |ui| {
            ui.add_space(4.0);
            page::grid(ui, items.len(), page::LANDSCAPE.x, |ui, i| {
                let c = super::catalog::item_card(ui, v, &items[i], false);
                if c.more {
                    more = Some(items[i].clone());
                } else if c.open {
                    play = Some(items[i].clone());
                }
            });
        }),
    }
    if let Some(item) = play {
        v.actions.push(Action::OpenList(
            vec![super::catalog::request(&source, &item, false)],
            0,
        ));
    }
    if let Some(item) = more {
        v.actions.push(Action::OpenSourceAt {
            source,
            item: Box::new(item),
            back_to: Screen::History,
        });
    }
    v.state.server_history = Some(history);
}

/// The playlists, read again after a change.
fn playlists(v: &mut View) -> Vec<Playlist> {
    v.state
        .playlists
        .get_or_insert_with(|| v.services.library.playlists().unwrap_or_default())
        .clone()
}

/// A grid of playlists on a scrolling page of its own; the one clicked.
fn cards(ui: &mut egui::Ui, v: &mut View, lists: &[Playlist]) -> Option<PlaylistId> {
    page::scroll(ui, |ui| grid_of(ui, v, lists))
}

/// A grid of playlists; the one clicked.
fn grid_of(ui: &mut egui::Ui, v: &mut View, lists: &[Playlist]) -> Option<PlaylistId> {
    let mut picked = None;
    page::grid(ui, lists.len(), page::LANDSCAPE.x, |ui, i| {
        let p = &lists[i];
        let thumb = p.first.as_ref().and_then(MediaRecord::picture);
        let facts = tr!("count-videos", count = p.item_count);
        let c = page::card(
            ui,
            v.thumbs,
            &Card {
                thumb: thumb.as_deref(),
                placeholder: icons::PLAYLIST,
                tint: theme::ACCENT_HOVER,
                title: &p.name,
                facts: &facts,
                play: false,
                ..Card::default()
            },
        );
        if c.open {
            picked = Some(p.id);
        }
    });
    picked
}

/// A name field and the button making a playlist of `reqs` (none: empty).
fn new_playlist(ui: &mut egui::Ui, v: &mut View, reqs: &[crate::playback::OpenRequest]) {
    ui.horizontal(|ui| {
        ui.add(
            egui::TextEdit::singleline(&mut v.state.playlist_name)
                .margin(egui::Margin::symmetric(12, 9))
                .hint_text(tr!("playlists-new"))
                .font(theme::font(Weight::Regular, 17.0))
                .desired_width(360.0),
        );
        let name = v.state.playlist_name.trim().to_string();
        let label = if reqs.is_empty() {
            tr!("playlists-make")
        } else {
            tr!("playlists-make-and-add")
        };
        if ui
            .add_enabled_ui(!name.is_empty(), |ui| {
                widgets::button_sized(ui, Some(icons::PLUS), label, Kind::Primary, 44.0)
            })
            .inner
            .clicked()
        {
            v.actions.push(Action::AddToPlaylist {
                playlist: None,
                name,
                reqs: reqs.to_vec(),
            });
        }
    });
}

/// A playlist's page: its videos in order, played through from the one
/// clicked, with moving, taking out, renaming and deleting.
fn playlist(ui: &mut egui::Ui, v: &mut View, id: PlaylistId) {
    let Some(p) = playlists(v).into_iter().find(|p| p.id == id) else {
        v.state.playlist = None;
        return;
    };
    if v.state
        .playlist_items
        .as_ref()
        .is_none_or(|(i, _)| *i != id)
    {
        let items = v.services.library.playlist_items(id).unwrap_or_default();
        v.state.playlist_items = Some((id, items));
    }
    let entries = v
        .state
        .playlist_items
        .as_ref()
        .map(|(_, e)| e.clone())
        .unwrap_or_default();
    let reqs: Vec<_> = entries
        .iter()
        .map(|e| super::library::request(&e.record))
        .collect();
    let armed = v
        .state
        .delete_armed
        .is_some_and(|t| t.elapsed().as_secs_f32() < 4.0);
    let facts = tr!("count-videos", count = entries.len());
    let back = page::header(ui, &p.name, Some(&facts), |ui| {
        let (label, kind) = if armed {
            (tr!("playlists-delete-confirm"), Kind::Danger)
        } else {
            (tr!("playlists-delete"), Kind::Secondary)
        };
        if widgets::button_sized(ui, Some(icons::TRASH), label, kind, 44.0).clicked() {
            if armed {
                v.actions.push(Action::DeletePlaylist(id));
                v.state.delete_armed = None;
            } else {
                v.state.delete_armed = Some(std::time::Instant::now());
            }
        }
        if armed {
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_millis(500));
        }
        if widgets::button_sized(
            ui,
            Some(icons::PENCIL_SIMPLE),
            tr!("playlists-rename"),
            Kind::Secondary,
            44.0,
        )
        .clicked()
        {
            v.state.renaming = true;
            v.state.playlist_name = p.name.clone();
        }
        if !reqs.is_empty()
            && widgets::button_sized(
                ui,
                Some(icons::PLAY),
                tr!("common-play-all"),
                Kind::Primary,
                44.0,
            )
            .clicked()
        {
            v.actions.push(Action::PlayAll(reqs.clone(), 0));
        }
    });
    if back {
        v.state.playlist = None;
        v.state.renaming = false;
        return;
    }
    if v.state.renaming {
        ui.horizontal(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut v.state.playlist_name)
                    .margin(egui::Margin::symmetric(12, 9))
                    .font(theme::font(Weight::Regular, 17.0))
                    .desired_width(360.0),
            );
            let name = v.state.playlist_name.trim().to_string();
            if ui
                .add_enabled_ui(!name.is_empty(), |ui| {
                    widgets::button_sized(
                        ui,
                        Some(icons::CHECK),
                        tr!("common-save"),
                        Kind::Primary,
                        44.0,
                    )
                })
                .inner
                .clicked()
            {
                v.actions.push(Action::RenamePlaylist(id, name));
                v.state.renaming = false;
                v.state.playlist_name.clear();
            }
            if widgets::button_sized(ui, None, tr!("common-cancel"), Kind::Ghost, 44.0).clicked() {
                v.state.renaming = false;
                v.state.playlist_name.clear();
            }
        });
        ui.add_space(8.0);
    }
    if entries.is_empty() {
        page::empty_state(
            ui,
            icons::PLAYLIST,
            tr!("playlists-empty"),
            tr!("playlists-empty-desc"),
            |_| {},
        );
        return;
    }
    let n = entries.len();
    page::scroll(ui, |ui| {
        ui.add_space(4.0);
        for (i, e) in entries.iter().enumerate() {
            let thumb = e.record.picture();
            let facts = super::library::facts(&e.record, v.settings.default_format);
            let resp = page::list_row(
                ui,
                v.thumbs,
                thumb.as_deref(),
                &widgets::display_title(&e.record.title),
                &facts,
                false,
                (i == 0, i + 1 == n),
            );
            // Positions count entries hidden by a lock, so a move lands
            // beside the neighbour shown.
            if resp.open {
                v.actions.push(Action::PlayAll(reqs.clone(), i));
            } else if resp.up {
                v.actions.push(Action::PlaylistMove(
                    id,
                    e.position,
                    entries[i - 1].position,
                ));
            } else if resp.down {
                v.actions.push(Action::PlaylistMove(
                    id,
                    e.position,
                    entries[i + 1].position,
                ));
            } else if resp.remove {
                v.actions.push(Action::PlaylistRemove(id, e.position));
            }
            ui.add_space(4.0);
        }
    });
}

/// Adding videos to a playlist: a new one, or one of yours. A single video
/// also gets Play, Play next and Add to queue (a folder's row opens this).
pub fn picker(ui: &mut egui::Ui, v: &mut View) {
    let Some(picking) = v.state.picking.take() else {
        return;
    };
    let subtitle = match picking.reqs.len() {
        1 => widgets::display_title(&picking.title),
        n => format!("{}: {}", picking.title, tr!("count-videos", count = n)),
    };
    let back = page::header(ui, tr!("playlists-picker-title"), Some(&subtitle), |_| {});
    if back {
        return;
    }
    v.state.picking = Some(picking);
    let reqs = v
        .state
        .picking
        .as_ref()
        .map(|p| p.reqs.clone())
        .unwrap_or_default();
    if let [req] = reqs.as_slice() {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 12.0;
            if widgets::button(ui, Some(icons::PLAY), tr!("player-play"), Kind::Primary).clicked() {
                v.actions.push(Action::Open(req.clone()));
                v.state.picking = None;
            }
            if widgets::button(
                ui,
                Some(icons::SKIP_FORWARD),
                tr!("jf-play-next"),
                Kind::Secondary,
            )
            .clicked()
            {
                v.actions.push(Action::Queue {
                    req: req.clone(),
                    next: true,
                });
                v.state.picking = None;
            }
            if widgets::button(
                ui,
                Some(icons::QUEUE),
                tr!("jf-add-to-queue"),
                Kind::Secondary,
            )
            .clicked()
            {
                v.actions.push(Action::Queue {
                    req: req.clone(),
                    next: false,
                });
                v.state.picking = None;
            }
        });
        ui.add_space(18.0);
    }
    new_playlist(ui, v, &reqs);
    ui.add_space(14.0);
    let lists = playlists(v);
    if !lists.is_empty() {
        widgets::section_label(ui, tr!("playlists-or-add-to"));
        ui.add_space(6.0);
        if let Some(id) = cards(ui, v, &lists) {
            v.actions.push(Action::AddToPlaylist {
                playlist: Some(id),
                name: String::new(),
                reqs,
            });
        }
    }
}
