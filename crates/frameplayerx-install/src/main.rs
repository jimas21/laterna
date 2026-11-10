//! Command-line entry point for `frameplayerx-install`.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Args, Parser, Subcommand};
use fp_updater::Channel;
use frameplayerx_install::device::DeviceInfo;
use frameplayerx_install::ops::{
    self, DEFAULT_INSTALL_DIR, InstallOptions, ShortcutMethod, UninstallOptions,
};
use frameplayerx_install::pair;
use frameplayerx_install::remote::SshRemote;
use frameplayerx_install::shortcut::DEFAULT_APP_NAME;
use frameplayerx_install::{InstallError, Result};

/// Install Laterna on a Steam Frame over SSH.
///
/// Before the first run: turn on Developer Mode on the headset. Then simply
/// run `frameplayerx-install`; if this PC is not paired yet it offers to pair
/// (or run `frameplayerx-install pair <headset IP>` first). Pairings made by
/// Frame Control, FrameDrop or Valve's SteamOS Devkit Client work too.
#[derive(Debug, Parser)]
#[command(version, args_conflicts_with_subcommands = true)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,

    #[command(flatten)]
    install: InstallArgs,
}

/// How to reach the headset (accepted by every command).
#[derive(Debug, Args)]
struct Connection {
    /// The headset: an ssh alias (Frame Control creates `frame`) or
    /// user@address.
    #[arg(long, default_value = "frame")]
    host: String,

    /// Extra ssh setting, e.g. `Port=2222` or `IdentityFile=~/.ssh/frame`
    /// (repeatable).
    #[arg(long = "ssh-option", value_name = "KEY=VALUE")]
    ssh_options: Vec<String>,
}

impl Connection {
    fn remote(&self) -> Result<SshRemote> {
        let remote = SshRemote::new(&self.host, &self.ssh_options)?;
        remote.check_tools()?;
        Ok(remote)
    }
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Install or update Laterna (the default when no command is given).
    Install(InstallArgs),
    /// Remove Laterna and its Steam library entry.
    Uninstall(UninstallArgs),
    /// Show what is installed on the headset.
    Status(StatusArgs),
    /// Pair this PC with the headset so ssh works (sets up the `frame`
    /// alias). Open Steam Settings > Developer > Pair new host on the
    /// headset first.
    Pair(PairArgs),
}

#[derive(Debug, Args)]
struct PairArgs {
    /// The headset's IP address (or a host name that resolves to it).
    address: String,
}

#[derive(Debug, Args)]
struct InstallArgs {
    #[command(flatten)]
    conn: Connection,

    /// Install this release zip instead of downloading the latest one.
    #[arg(long, value_name = "FILE")]
    zip: Option<PathBuf>,

    /// Install folder on the headset, relative to the home folder.
    /// Devkit-style installs use `devkit-game/frameplayerx`.
    #[arg(long, default_value = DEFAULT_INSTALL_DIR, value_name = "FOLDER")]
    dir: String,

    /// Release channel to download from.
    #[arg(long, default_value = "stable", value_parser = parse_channel)]
    channel: Channel,

    /// Signed release manifest to download from, instead of the newest
    /// release on the channel.
    #[arg(long, value_name = "URL")]
    manifest_url: Option<String>,

    /// Name shown in the Steam library.
    #[arg(long, default_value = DEFAULT_APP_NAME)]
    name: String,

    /// How to add Laterna to the Steam library.
    #[arg(long, value_enum, default_value_t = ShortcutMethod::Auto)]
    shortcut: ShortcutMethod,

    /// Restart Steam afterwards (runs `steam -shutdown`; Gaming Mode
    /// starts it again) so the new library entry appears right away.
    #[arg(long)]
    restart_steam: bool,

    /// Copy the files even if this version is already installed.
    #[arg(long)]
    reinstall: bool,

    /// Install even if the device does not look like a Steam Frame.
    #[arg(long)]
    force: bool,
}

#[derive(Debug, Args)]
struct UninstallArgs {
    #[command(flatten)]
    conn: Connection,

    /// Install folder on the headset, relative to the home folder.
    #[arg(long, default_value = DEFAULT_INSTALL_DIR, value_name = "FOLDER")]
    dir: String,

    /// Name Laterna was given in the Steam library.
    #[arg(long, default_value = DEFAULT_APP_NAME)]
    name: String,

    /// Also delete Laterna's settings, library database and caches.
    #[arg(long)]
    purge: bool,

    /// Restart Steam afterwards so the library updates right away.
    #[arg(long)]
    restart_steam: bool,

    /// Run even if the device does not look like a Steam Frame.
    #[arg(long)]
    force: bool,
}

#[derive(Debug, Args)]
struct StatusArgs {
    #[command(flatten)]
    conn: Connection,

    /// Install folder on the headset, relative to the home folder.
    #[arg(long, default_value = DEFAULT_INSTALL_DIR, value_name = "FOLDER")]
    dir: String,
}

fn parse_channel(s: &str) -> std::result::Result<Channel, String> {
    s.parse()
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("\nError: {e}");
            if let Some(hint) = e.hint() {
                eprintln!("\n{hint}");
            }
            ExitCode::FAILURE
        }
    }
}

fn run(cli: Cli) -> Result<()> {
    match cli.command {
        None => install(cli.install),
        Some(Command::Install(args)) => install(args),
        Some(Command::Uninstall(args)) => {
            let mut remote = args.conn.remote()?;
            let device = ops::connect(&mut remote, args.force)?;
            ops::uninstall(
                &mut remote,
                &device,
                &UninstallOptions {
                    dir: args.dir,
                    app_name: args.name,
                    purge: args.purge,
                    restart_steam: args.restart_steam,
                },
            )
        }
        Some(Command::Status(args)) => {
            let mut remote = args.conn.remote()?;
            let device = ops::connect(&mut remote, true)?;
            ops::status(&mut remote, &device, &args.dir).map(|_| ())
        }
        Some(Command::Pair(args)) => {
            pair::pair(&args.address, pair::interactive())?;
            let mut remote = SshRemote::new(pair::FRAME_ALIAS, &[])?;
            remote.check_tools()?;
            ops::connect(&mut remote, true)?;
            println!("\nDone. Now run frameplayerx-install to install Laterna.");
            Ok(())
        }
    }
}

/// Connects for an install. When login fails in a way pairing can fix and
/// someone is at the keyboard, offers to pair right away and then connects
/// through the new `frame` alias.
fn connect_or_pair(conn: &Connection, force: bool) -> Result<(SshRemote, DeviceInfo)> {
    let mut remote = conn.remote()?;
    let err = match ops::connect(&mut remote, force) {
        Ok(device) => return Ok((remote, device)),
        Err(e) if pair::pairing_might_help(&e) && pair::interactive() => e,
        Err(e) => return Err(e),
    };
    println!("\n{err}");
    println!("\nThis PC does not seem to be paired with the headset yet.");
    let yes = pair::ask("Pair it now? [Y/n] ").is_some_and(|a| {
        a.is_empty() || a.eq_ignore_ascii_case("y") || a.eq_ignore_ascii_case("yes")
    });
    if !yes {
        return Err(err);
    }
    let known = pair::address_for_host(&conn.host, &pair::read_ssh_config());
    let question = match &known {
        Some(a) => format!("Headset IP address [{a}]: "),
        None => "Headset IP address (see its Wi-Fi network details): ".to_string(),
    };
    let address = match (pair::ask(&question), known) {
        (Some(a), _) if !a.is_empty() => a,
        (_, Some(k)) => k,
        _ => return Err(err),
    };
    pair::pair(&address, true)?;
    println!("Connecting through the new `{}` alias.", pair::FRAME_ALIAS);
    let mut remote = SshRemote::new(pair::FRAME_ALIAS, &conn.ssh_options)?;
    let device = ops::connect(&mut remote, force)?;
    Ok((remote, device))
}

fn install(args: InstallArgs) -> Result<()> {
    // Connect first: a pairing problem should show up before a long
    // download.
    let (mut remote, device) = connect_or_pair(&args.conn, args.force)?;
    let zip = match args.zip {
        Some(z) => {
            if !z.is_file() {
                return Err(InstallError::BadPath(format!(
                    "{} is not a file",
                    z.display()
                )));
            }
            z
        }
        None => {
            let cache = std::env::temp_dir().join("frameplayerx-install");
            ops::fetch_latest(args.manifest_url.as_deref(), args.channel, &cache)?
        }
    };
    let version = ops::install_zip(
        &mut remote,
        &device,
        &zip,
        &InstallOptions {
            dir: args.dir,
            app_name: args.name.clone(),
            shortcut: args.shortcut,
            restart_steam: args.restart_steam,
            reinstall: args.reinstall,
        },
    )?;
    println!(
        "\nDone. Laterna {version} is on the headset: find \"{}\" in the Steam library \
         (Library > Non-Steam).",
        args.name
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn cli_is_consistent() {
        Cli::command().debug_assert();
    }

    #[test]
    fn default_command_is_install() {
        let cli = Cli::try_parse_from(["frameplayerx-install"]).unwrap();
        assert!(cli.command.is_none());
        assert_eq!(cli.install.conn.host, "frame");
        assert_eq!(cli.install.dir, "frameplayerx");
        assert_eq!(cli.install.channel, Channel::Stable);
        assert_eq!(cli.install.manifest_url, None);

        let cli = Cli::try_parse_from([
            "frameplayerx-install",
            "--host",
            "steam@192.168.1.20",
            "--zip",
            "fp.zip",
            "--channel",
            "beta",
            "--shortcut",
            "vdf",
            "--restart-steam",
        ])
        .unwrap();
        assert_eq!(cli.install.conn.host, "steam@192.168.1.20");
        assert_eq!(cli.install.zip, Some(PathBuf::from("fp.zip")));
        assert_eq!(cli.install.channel, Channel::Beta);
        assert_eq!(cli.install.shortcut, ShortcutMethod::Vdf);
        assert!(cli.install.restart_steam);
    }

    #[test]
    fn subcommands_parse() {
        let cli = Cli::try_parse_from([
            "frameplayerx-install",
            "uninstall",
            "--purge",
            "--host",
            "deck",
        ])
        .unwrap();
        match cli.command {
            Some(Command::Uninstall(a)) => {
                assert!(a.purge);
                assert_eq!(a.dir, "frameplayerx");
                assert_eq!(a.conn.host, "deck");
            }
            other => panic!("{other:?}"),
        }
        let cli = Cli::try_parse_from([
            "frameplayerx-install",
            "status",
            "--ssh-option",
            "Port=2222",
            "--dir",
            "devkit-game/frameplayerx",
        ])
        .unwrap();
        match cli.command {
            Some(Command::Status(a)) => {
                assert_eq!(a.conn.ssh_options, ["Port=2222"]);
                assert_eq!(a.dir, "devkit-game/frameplayerx");
            }
            other => panic!("{other:?}"),
        }
        let cli = Cli::try_parse_from(["frameplayerx-install", "pair", "192.168.0.68"]).unwrap();
        match cli.command {
            Some(Command::Pair(a)) => assert_eq!(a.address, "192.168.0.68"),
            other => panic!("{other:?}"),
        }
        assert!(Cli::try_parse_from(["frameplayerx-install", "pair"]).is_err());
        // Install options belong to install, not to other commands.
        assert!(Cli::try_parse_from(["frameplayerx-install", "--zip", "a.zip", "status"]).is_err());
        assert!(Cli::try_parse_from(["frameplayerx-install", "--channel", "nightly"]).is_err());
    }
}
