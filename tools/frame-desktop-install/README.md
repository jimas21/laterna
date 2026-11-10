# Installing from Desktop Mode

Installs Laterna on a Steam Frame from its Desktop Mode, with no PC,
and adds it to the Steam library. Both files here are attached to every
release:

- `Install-FramePlayerX.desktop`: what people download. Opened in Desktop
  Mode, it downloads `frameplayerx-desktop-install.py` from the latest
  stable release and runs it in a Konsole window, which stays open so the
  result can be read. Desktop Mode asks once whether to trust the file.
- `Uninstall-FramePlayerX.desktop`: the same, running it with `--remove`.
- `frameplayerx-desktop-install.py`: the installer. It uses only the Python
  3 that SteamOS ships.

What the installer does:

1. Finds the highest-versioned release on GitHub (stable only, unless
   `--beta`), downloads its zip and checks it against the SHA-256 GitHub
   recorded for it.
2. Unpacks the release next to `~/frameplayerx` and checks it, then swaps
   it in; if that fails, the installed version is put back. Nothing else
   changes before this, so a bad download or a full disk leaves what was
   installed.
3. Removes other copies of Laterna: their library entries and the
   folders they start from. Settings and the watch history live elsewhere
   and stay.
4. Adds the Steam library entry with its artwork. The Frame's desktop Steam
   has no "Add a Non-Steam Game" menu, so the installer adds it one of two
   ways (`--steam`):
   - **live**: through the Steam client's DevTools port (127.0.0.1:8080).
     Steam shows the entry at once.
   - **file**: by editing `userdata/<account>/config/shortcuts.vdf`, as
     `frameplayerx-install` does over SSH. Steam has to close first (the
     installer asks), and the entry appears back in Gaming Mode.
   - **auto** (the default): live, else file.

The entry, app id, `VR` tag and artwork names match
`crates/frameplayerx-install/src/shortcut.rs`, so the two installers update
each other's entry.

From a terminal:

```sh
python3 frameplayerx-desktop-install.py                  # latest stable release
python3 frameplayerx-desktop-install.py --beta           # newest release, betas too
python3 frameplayerx-desktop-install.py --zip ~/Downloads/frameplayerx-1.0.0-aarch64.zip
python3 frameplayerx-desktop-install.py --dir ~/Apps --name-suffix " (test)"
python3 frameplayerx-desktop-install.py --remove            # asks whether to keep your data
python3 frameplayerx-desktop-install.py --remove --purge    # deletes it too
```

A copy with `--name-suffix` gets its own library entry and leaves the other
copies alone.

`--remove` asks for confirmation, then deletes the install folder (and
`.old`), the library entry and its artwork, like `frameplayerx-install
uninstall`. It then asks whether to delete the settings, data and cache
folders too, which `--purge` does without asking. `FRAMEPLAYERX_INSTALL_YES=1`
answers the installer's questions with yes, except that one: data is only
deleted when someone says so.

## Tests

`test_frameplayerx_desktop_install.py` covers choosing the release, the
binary VDF round trip, the library entry, the zip unpacking, uninstalling
and both launchers. CI runs it; locally, run `python3 -m unittest` in this folder.
