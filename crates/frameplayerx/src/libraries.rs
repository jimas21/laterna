//! Libraries: folders on the headset or on a microSD card or USB drive,
//! each a source of its own whose videos are scanned into the media
//! library. A card's library knows the card by its filesystem ID
//! ([`fp_core::volume`]), so two cards with the same folders stay apart.

use fp_core::volume::{self, Volume};
use fp_sources::{CardRef, LibraryConfig, SourceConfig};
use std::path::{Path, PathBuf};

/// The library of the folder at `path`: on a card, by the card's ID.
pub fn config_for(id: String, name: String, path: &Path) -> Result<LibraryConfig, String> {
    if path.is_absolute() && !path.is_dir() {
        return Err(tr!("library-error-no-folder").into());
    }
    config_in(&volume::mounted(), id, name, path)
}

/// [`config_for`] with the cards `volumes` mounted.
pub fn config_in(
    volumes: &[Volume],
    id: String,
    name: String,
    path: &Path,
) -> Result<LibraryConfig, String> {
    if !path.is_absolute() {
        return Err(tr!("library-error-full-path").into());
    }
    let path: PathBuf = path.components().collect();
    let removable = Path::new(volume::REMOVABLE_ROOT);
    if removable.starts_with(&path) {
        return Err(tr!("library-error-choose-folder").into());
    }
    let text = path.to_str().ok_or(tr!("library-error-name"))?;
    let stable = volume::stable_in(volumes, text);
    if let Some((card, folder)) = volume::split(&stable) {
        let label = volumes
            .iter()
            .find(|v| v.id.as_deref() == Some(card))
            .map(Volume::name)
            .unwrap_or_default();
        return Ok(LibraryConfig {
            id,
            name,
            folder: folder.into(),
            card: Some(CardRef {
                id: card.to_string(),
                label,
            }),
        });
    }
    if path.starts_with(removable) {
        return Err(tr!("library-error-no-id").into());
    }
    Ok(LibraryConfig {
        id,
        name,
        folder: path,
        card: None,
    })
}

/// The name of the library in `others` whose folder holds `config`'s, is
/// inside it, or is the same.
pub fn overlap<'a>(config: &LibraryConfig, others: &'a [SourceConfig]) -> Option<&'a str> {
    let root = config.root();
    others
        .iter()
        .filter_map(|c| match c {
            SourceConfig::Library(l) if l.id != config.id => Some(l),
            _ => None,
        })
        .find(|l| {
            let other = l.root();
            within(&root, &other) || within(&other, &root)
        })
        .map(|l| l.name.as_str())
}

fn within(inner: &str, outer: &str) -> bool {
    inner
        .strip_prefix(outer)
        .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
}

/// Where to scan a library now: its folder, and what its videos are
/// located under when it is on a card. `None` while the card is out.
pub fn scan_target(config: &LibraryConfig) -> Option<(PathBuf, Option<String>)> {
    let root = config.root();
    let path = PathBuf::from(volume::resolve(&root)?.as_ref());
    Some((path, config.card.is_some().then_some(root)))
}

/// Whether a library's folder can be reached: always on the headset, on a
/// card while it is in.
pub fn present(config: &LibraryConfig) -> bool {
    volume::present(&config.root())
}

/// Folders worth offering for a new library: the usual video folders and
/// the top of each card, unless a library has them already.
pub fn suggestions(configs: &[SourceConfig]) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
        for d in ["Videos", "Downloads", "Movies"] {
            out.push(home.join(d));
        }
    }
    out.extend(
        volume::mounted()
            .into_iter()
            .filter(|v| v.id.is_some())
            .map(|v| v.mount),
    );
    out.retain(|p| {
        config_for(String::new(), String::new(), p).is_ok_and(|c| overlap(&c, configs).is_none())
    });
    out
}

/// A location for people: where a file on a card is now, or that the card
/// is out.
pub fn display_location(location: &str) -> String {
    match volume::resolve(location) {
        Some(path) => path.into_owned(),
        None => match volume::split(location) {
            Some((_, "")) | None => tr!("location-card-out").to_string(),
            Some((_, path)) => tr!("location-on-card-out", path = path),
        },
    }
}

/// A name for the library of `path`: its folder's, or the card's for the
/// top of a card.
pub fn default_name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn card(mount: &str, id: &str) -> Volume {
        Volume {
            mount: PathBuf::from(mount),
            device: "/dev/mmcblk0p1".into(),
            id: Some(id.into()),
        }
    }

    fn lib(volumes: &[Volume], id: &str, path: &str) -> LibraryConfig {
        config_in(volumes, id.into(), id.into(), Path::new(path)).unwrap()
    }

    #[test]
    fn folders_on_cards_are_kept_by_the_card() {
        let v = [card("/run/media/deck/SDCARD", "AAAA-0001")];
        let c = lib(&v, "clips", "/run/media/deck/SDCARD/Clips/");
        assert_eq!(c.folder, PathBuf::from("Clips"));
        assert_eq!(
            c.card,
            Some(CardRef {
                id: "AAAA-0001".into(),
                label: "SDCARD".into()
            })
        );
        assert_eq!(c.root(), "volume:AAAA-0001/Clips");
        let whole = lib(&v, "card", "/run/media/deck/SDCARD");
        assert_eq!(whole.root(), "volume:AAAA-0001");

        let home = lib(&v, "movies", "/home/deck/Videos/Movies");
        assert_eq!(home.card, None);
        assert_eq!(home.root(), "/home/deck/Videos/Movies");

        let refused = |p: &str| config_in(&v, "x".into(), "x".into(), Path::new(p)).is_err();
        assert!(refused("Videos"), "relative");
        assert!(refused("/run/media/deck"), "holds the cards");
        assert!(refused("/"), "holds the cards");
        assert!(refused("/run/media/deck/USB/x"), "a drive without an ID");
    }

    #[test]
    fn libraries_may_not_overlap() {
        let v = [
            card("/run/media/deck/SDCARD", "AAAA-0001"),
            card("/run/media/deck/USB", "BBBB-0002"),
        ];
        let others: Vec<SourceConfig> = [
            lib(&v, "movies", "/home/deck/Videos/Movies"),
            lib(&v, "clips", "/run/media/deck/SDCARD/Clips"),
        ]
        .into_iter()
        .map(SourceConfig::Library)
        .collect();
        let check = |p: &str| overlap(&lib(&v, "new", p), &others);
        assert_eq!(check("/home/deck/Videos"), Some("movies"), "holds one");
        assert_eq!(check("/home/deck/Videos/Movies/Old"), Some("movies"));
        assert_eq!(check("/home/deck/Videos/Movies"), Some("movies"));
        assert_eq!(check("/home/deck/Videos/Movies 2"), None);
        assert_eq!(check("/run/media/deck/SDCARD"), Some("clips"));
        assert_eq!(check("/run/media/deck/SDCARD/Trips"), None);
        assert_eq!(check("/run/media/deck/USB/Clips"), None, "another card");
        // Editing a library doesn't clash with itself.
        assert_eq!(
            overlap(&lib(&v, "movies", "/home/deck/Videos/Movies/New"), &others),
            None
        );
    }
}
