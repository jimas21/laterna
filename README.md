<p align="center">
  <img src="assets/logo.svg" alt="Laterna logo" width="160" />
</p>

# Laterna

[![CI](https://github.com/jimas21/laterna/actions/workflows/ci.yml/badge.svg?branch=environments&event=push)](https://github.com/jimas21/laterna/actions/workflows/ci.yml)
[![Latest Release](https://img.shields.io/github/v/release/jimas21/laterna?sort=semver&style=flat&logo=github&label=release)](https://github.com/jimas21/laterna/releases/latest)
[![Latest Beta](https://img.shields.io/github/v/release/jimas21/laterna?include_prereleases&sort=date&style=flat&logo=github&label=beta&color=orange)](https://github.com/jimas21/laterna/releases)
[![License: Apache-2.0](https://img.shields.io/badge/license-Apache--2.0-blue?style=flat)](LICENSE)

A media player for the Valve Steam Frame that runs natively on the headset: flat, 180°, 360° and fisheye video in 2D or 3D, from the headset, a card, a USB drive or a media server.

The name comes from the *laterna magica*, the magic lantern: the first projector, which threw pictures onto any wall.

> [!NOTE]
> **Where it comes from.** Laterna is a fork of [FramePlayerX](https://github.com/remotesojourner/frameplayerx) by remotesojourner, which is itself a fork of [FramePlayer](https://github.com/yellkell/frameplayer) by [yellkell](https://github.com/yellkell), who created the original player and released it as open source. Thank you both. Everything here is Apache-2.0, like the projects it builds on. Laterna's releases and in-app updates come from this repository, signed with its own key.

## What Laterna adds

### Picture quality
- **A sharper, steadier picture**: smooth screen edges, and no shimmer on screens that are far away or seen at an angle
- **Auto-pause**: taking the headset off pauses the video, and it stays paused until you press play

### Handling the screen
- **Move it by hand**: grab the screen or any menu and push it away, pull it closer or move it around, without opening a menu
- **Smooth to move**: dragged screens and menus follow your hand without lag
- **Settings apply at once**: changing the screen's default position in Settings moves the video you're watching

### Places to watch *(in progress)*
- **Environments**: watch from a seat in a cinema, a home theater or a studio, instead of empty space
- **House lights** that dim when the film starts

### Plex *(in progress)*
- **Your server's own rows**, with its posters and artwork
- **Plex Home users**: switch between the people on your account
- **Sign in once** and every server on your account is there
- **Playlists and watch history** from your server

### Earbuds *(next beta)*
- **AirPods and other earbuds**: battery, noise control, and pause when you take one out. Uses [Tragus](https://github.com/jimas21/tragus), our earbuds companion for the Frame, installed from Laterna's Settings

---

## Features

- **VR and flat videos**: plays 180°, 360°, fisheye and flat videos, in 2D or 3D. The format is detected automatically, and you can change it for any video
- **Hardware decoding**: HEVC (H.265) in 8 and 10 bits and H.264, both up to 8K, play on the Frame's own video decoder. Anything else, AV1 and VP9 included, plays with software decoding
- **3D Blu-ray**: disc images (`.iso`), MKV rips and the disc's `.ssif` files play in full 3D (H.264 MVC) on the hardware decoder
- **Frame sequential 3D**: files whose eyes take turns frame by frame play in full 3D at the film's own frame rate
- **Picture adjustments**: position, zoom, depth, picture settings and fisheye lens calibration, saved for each video, and a screen that follows your head
- **Videos on the headset**: browse any folder on the headset, a microSD card or a USB drive, including cards formatted on Windows or a Mac, and make libraries of your folders, with thumbnails, filters and sorting
- **Media servers**: browse and play from Jellyfin, Emby, Plex, any server with a DeoVR feed or HereSphere API, network shares (SMB), WebDAV, DLNA servers and web folders. Server transcoding and watch state sync are supported
- **Watch history, Up next and playlists**: carry on where you left off, queue what plays next, shuffle the queue, autoplay it, loop a video or the whole queue, keep playlists of videos from any source, and mark favorites and ratings
- **PIN locks**: hide a source behind a PIN until you unlock it
- **Passthrough videos**: see your room around the people in passthrough videos, and behind green or blue screens
- **Subtitles and spatial audio**: subtitles from a file or inside the video, picked in your language and shown in any script, and 360° audio that follows your head
- **Haptic devices**: optional, off until turned on; syncs supported haptic devices with videos that have scripts
- **Phone remote**: open the address the headset shows on your phone and enter its code, then control the player and type into the headset from the phone. Apps made for DeoVR's remote work too
- **Custom controls**: choose what each controller button does
- **Languages**: English, German, Spanish, French, Japanese and Simplified Chinese, following the language Steam is set to; the phone remote follows the headset
- **Updates**: checks for a new version when it starts, and installs it from Settings when you choose to, on a Stable or a Beta channel
- **No account and no tracking**: Laterna connects to the servers you add and to GitHub to check for updates. Linking Plex also contacts Plex's account services to sign in, discover servers and renew access. After a crash it asks whether to send a crash report, shows what's in it (no video names, folders or server addresses) and sends nothing unless you choose to

---

## Requirements

| Requirement | Notes |
|---|---|
| Valve Steam Frame | Developer Mode only to install with Frame Control or a PC |
| A PC (Windows, macOS or Linux) | Optional: only to install from a PC |
| Media server | Optional: Jellyfin, Emby or Plex, a DeoVR feed or HereSphere API, a NAS share, WebDAV or DLNA |

---

## Quick Start

<p align="center">
  <a href="https://jimas21.github.io/laterna/Install-Laterna.desktop"><img src="assets/install-button.svg" alt="Install in Desktop Mode" width="240" /></a>
  <a href="https://jimas21.github.io/laterna/frame-control.html"><img src="assets/frame-control-button.svg" alt="Install with Frame Control" width="240" /></a>
</p>

> [!IMPORTANT]
> Laterna is in testing. Until its first stable release, the Desktop Mode button installs the newest beta.

1. **Install** with one of the buttons above, or from a terminal:

   - **Desktop Mode, on the headset**: open this page in the headset's browser and press **Desktop Mode** to download the installer. Open it from your Downloads folder and confirm when Desktop Mode asks whether to trust it. A terminal window opens, installs the latest release and adds Laterna to your Steam library. It may ask to close Steam for a moment; Laterna then shows up in Gaming Mode.
   - **Frame Control, from a PC**: turn on Developer Mode (Steam Settings › System › Developer Mode) and pair the headset once with [Frame Control](https://github.com/saphid/steam-frame). Then press its button in your PC's browser and it installs the latest release on the headset.
   - **From a PC's terminal**: turn on Developer Mode, download `laterna-install` for your PC from the [release page](https://github.com/jimas21/laterna/releases/latest), then:

     ```bash
     laterna-install pair <headset-ip>    # once: the address in Steam Settings › Internet on the headset
     laterna-install                      # installs the newest release; add --channel beta for betas
     ```

     For `pair`, open Steam Settings › Developer › Pair new host on the headset and keep it showing.

2. **Start it** from the Steam library on the headset. Browse the headset and its cards under **On this device**, and add a library of a folder, or a media server, under **Sources › Add source**.

To update later, open **Settings › Updates** and press **Download and install** when a new version is listed, then **Restart now**. To remove it, see [Uninstall](docs/USER-GUIDE.md#uninstall) in the user guide. Laterna installs over an existing FramePlayerX and keeps its settings and libraries.

---

## Documentation

| Guide | What's in it |
|---|---|
| [User guide](docs/USER-GUIDE.md) | Installing, updating and uninstalling, sources, browsing, PIN locks, video formats, controls, passthrough, haptics, remote control and troubleshooting |
| [Development](docs/DEVELOPMENT.md) | Building and testing, how the code is organised, running without a headset (Monado and the preview harness), and releasing |
| [Contributing](CONTRIBUTING.md) | What to do before opening a pull request, what it has to say and how to test it |

---

## Help translate

Laterna comes in English, German, Spanish, French, Japanese and Simplified Chinese. It shares its translations with FramePlayerX: all but English started as machine drafts, so some wording is likely to be wrong or awkward, and corrections are welcome.

- **Correct or translate** on [Codeberg Translate](https://translate.codeberg.org/projects/frameplayerx/), where FramePlayerX's translations live. No Git needed: sign in with a Codeberg account, pick a language and edit its messages. Corrections there reach Laterna when it takes in FramePlayerX's changes.
- **Laterna's own features** (places, Plex additions, earbuds) are in English first. To translate them, or to report a mistake, open a [Translation problem](https://github.com/jimas21/laterna/issues/new?template=translation.yml) issue with the language, where it shows and the wording you'd expect.
- **Add a language**: ask in an issue. Right-to-left languages (Arabic, Hebrew) and scripts that need shaping (Thai, Hindi and other Indic scripts) can't be shown in the interface yet.

---

## Technology Stack

| Component | Technology |
|---|---|
| Language | Rust |
| VR | OpenXR, talking to the runtime directly (no Khronos loader) |
| Graphics | Vulkan through [ash](https://github.com/ash-rs/ash); the projections are drawn per pixel in a shader |
| Interface | [egui](https://github.com/emilk/egui) panels in VR, with the [Inter](https://rsms.me/inter/) font and [Phosphor](https://phosphoricons.com/) icons; subtitle text set by [cosmic-text](https://github.com/pop-os/cosmic-text) |
| Video | [FFmpeg](https://ffmpeg.org/) 7.1 for demuxing and software decoding, the Frame's V4L2 hardware decoder (H.264, and 8- and 10-bit HEVC), [dav1d](https://code.videolan.org/videolan/dav1d) for AV1 |
| Audio | ALSA |
| Storage | SQLite (videos, watch history, adjustments), JSON settings |
| Builds | `cargo zigbuild` for Linux ARM64 against glibc 2.28 |

---

## License

Laterna is licensed under the [Apache License, Version 2.0](LICENSE). It's a modified version of FramePlayerX by remotesojourner, which is a modified version of FramePlayer by yellkell; see [NOTICE](NOTICE). Bundled third-party libraries and fonts keep their own licences, and the controller pictures are renders of Valve's Steam Frame controller models; see [packaging/licenses](packaging/licenses/THIRD-PARTY.txt).

Laterna is an independent project, not affiliated with or endorsed by Valve, or by Plex. Steam and Steam Frame are trademarks of Valve Corporation; Plex is a trademark of Plex, Inc.

Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in Laterna by you shall be licensed under the Apache License, Version 2.0, without any additional terms or conditions.

---

## FAQ

### Does it run on other headsets?

No. Laterna is built only for the Steam Frame, so it can use the Frame's decoder, controllers and runtime directly instead of carrying everything a cross-platform player needs.

---

### Why does a video stutter?

Usually because it's AV1 or 4:2:2, which the Frame's hardware decoder can't play, so it's decoded on the CPU. The control bar says so. Converting it to HEVC once on a PC makes it play smoothly; the [user guide](docs/USER-GUIDE.md#videos-that-stutter-av1-and-422) has the commands.

---

### Does it work with my media server or NAS?

Yes, if it offers one of these: Jellyfin (sign in with Quick Connect or a password), Emby (a password), Plex (sign in with a linking code or an access token), a DeoVR feed or HereSphere API, SMB, WebDAV or DLNA. Add it under Sources › Add source.

---

### Will it be on the Steam store?

Not for now. The installers in [Quick Start](#quick-start) add it to your Steam library like a game.

---

### Why a separate fork?

Laterna goes further in a few directions: places to watch in, a fuller Plex experience, earbud support and picture-quality work tuned on the Frame. It keeps taking in FramePlayerX's updates, and offers fixes and features back to FramePlayerX where they fit, so both players get better.

---

### Do you plan to add new features?

Yes: the places to watch and the Plex work above are in progress now. Suggestions are welcome as issues.

---

## Screenshots

| Home | Player & adjustments |
| :---: | :---: |
| ![Home screen](assets/screenshots/home-watched.png) | ![Player and adjustments](assets/screenshots/player.png) |
| **Video library** | **Jellyfin server** |
| ![Video library](assets/screenshots/library.png) | ![Jellyfin server](assets/screenshots/jellyfin.png) |
| **Local folders** | **Video details** |
| ![Local folders](assets/screenshots/library-folders.png) | ![Video details](assets/screenshots/details.png) |
| **Up next queue** | **Controller settings** |
| ![Up next queue](assets/screenshots/up-next.png) | ![Controller settings](assets/screenshots/settings-controller.png) |

---

## A Note on AI

This project was built with extensive help from [Claude Code](https://claude.com/claude-code), as was FramePlayerX.
