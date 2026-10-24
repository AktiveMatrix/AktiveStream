//! AktiveStream for Windows: a client for macOS Screen Sharing with the macOS
//! keyboard profile.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod clipboard;
mod creds;
mod cursor;
mod diag;
mod native;
mod net;
mod picker;
mod render;
mod view;

use aktivestream_client::{config, tunnel};
use aktivestream_rfb::{ConnectOptions, Error as RfbError};
use windows::Win32::Foundation::ERROR_LOGON_FAILURE;
use windows::Win32::UI::HiDpi::{DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext};

const USAGE: &str = "usage: aktivestream <host-or-saved-name>[:port] [--user NAME] [--fullscreen] [--forget]\n       aktivestream --list\n\n\
  --user NAME            Mac account name to suggest in the login prompt\n\
  --fullscreen           start in full screen\n\
  --forget               delete the saved login for this host first\n\
  --tunnel SSH           reach the Mac through an ssh -L forward (VNC only; Tailscale needs none)\n\
  --list                 print saved connections (phone book) and exit\n\n\
diagnostics:\n\
  --password-env VAR     take the password from environment variable VAR (with --user);\n\
                         skips the prompt and Credential Manager\n\
  --screenshot PATH      with --exit-after, save the rendered client area as PNG\n\
  --exit-after SECS      close the session after SECS seconds and print statistics\n\
  --view-only            send no keyboard or mouse input (keys still reach --keylog)\n\
  --keylog PATH          log every keyboard-hook event, its routing and keysyms, and clipboard traffic\n\
  --native               connect to the Phase 2 native host (QUIC + HEVC) instead of VNC\n\
  --show-cursor          show the local cursor over the view (native hides it by default, as the\n\
                         remote cursor is already rendered into the video)\n\
  --no-scale-up          do not enlarge a remote screen smaller than the window; show it at 1:1,\n\
                         centred (the default scales it up to fill the window, aspect kept)";

pub struct Args {
    pub host: String,
    /// Additional targets (raw, unresolved) to open as extra switchable sessions.
    pub extra_hosts: Vec<String>,
    pub port: u16,
    pub user: Option<String>,
    pub fullscreen: bool,
    pub forget: bool,
    pub password_env: Option<String>,
    pub screenshot: Option<std::path::PathBuf>,
    pub exit_after: Option<u32>,
    pub view_only: bool,
    pub keylog: Option<std::path::PathBuf>,
    pub native: bool,
    pub tunnel: Option<String>,
    pub show_cursor: bool,
    pub no_scale_up: bool,
}

fn parse_args() -> Result<Args, String> {
    let mut host = None;
    let mut extra_hosts = Vec::new();
    let mut user = None;
    let mut fullscreen = false;
    let mut forget = false;
    let mut password_env = None;
    let mut screenshot = None;
    let mut exit_after = None;
    let mut view_only = false;
    let mut keylog = None;
    let mut native = false;
    let mut show_cursor = false;
    let mut no_scale_up = false;
    let mut list = false;
    let mut tunnel = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--user" => user = Some(args.next().ok_or("--user needs a name")?),
            "--fullscreen" => fullscreen = true,
            "--forget" => forget = true,
            "--view-only" => view_only = true,
            "--native" => native = true,
            "--show-cursor" => show_cursor = true,
            "--no-scale-up" => no_scale_up = true,
            "--list" => list = true,
            "--tunnel" => tunnel = Some(args.next().ok_or("--tunnel needs an ssh destination")?),
            "--keylog" => keylog = Some(args.next().ok_or("--keylog needs a path")?.into()),
            "--password-env" => password_env = Some(args.next().ok_or("--password-env needs a variable name")?),
            "--screenshot" => screenshot = Some(args.next().ok_or("--screenshot needs a path")?.into()),
            "--exit-after" => {
                let secs = args.next().ok_or("--exit-after needs seconds")?;
                exit_after = Some(secs.parse().map_err(|_| format!("bad --exit-after value {secs}"))?);
            }
            "-h" | "--help" => return Err(USAGE.to_string()),
            _ if arg.starts_with('-') => return Err(format!("unknown option {arg}\n\n{USAGE}")),
            _ if host.is_none() => host = Some(arg),
            // Additional hosts open as extra switchable sessions (RFB only).
            _ => extra_hosts.push(arg),
        }
    }
    if list {
        print_phone_book();
        std::process::exit(0);
    }
    let target = host.ok_or_else(|| USAGE.to_string())?;
    // Phone book: a saved connection name resolves to its host/port/mode/user/tunnel; otherwise
    // the argument is a host[:port].
    let (host, port, saved_native, saved_user, saved_tunnel) = match config::find(&target) {
        Some(c) => (c.host, c.port, c.mode == config::Mode::Native, c.username, c.tunnel),
        None => {
            let (h, p) = match target.rsplit_once(':') {
                Some((h, p)) if !h.contains(':') => {
                    (h.to_string(), p.parse().map_err(|_| format!("bad port in {target}"))?)
                }
                _ => (target, 5900),
            };
            (h, p, false, None, None)
        }
    };
    let native = native || saved_native;
    let user = user.or(saved_user);
    let tunnel = tunnel.or(saved_tunnel);
    Ok(Args {
        host,
        extra_hosts,
        port,
        user,
        fullscreen,
        forget,
        password_env,
        screenshot,
        exit_after,
        view_only,
        keylog,
        native,
        tunnel,
        show_cursor,
        no_scale_up,
    })
}

/// Prints saved connections. A GUI build has no console of its own, so attach the parent's.
fn print_phone_book() {
    unsafe {
        let _ = windows::Win32::System::Console::AttachConsole(windows::Win32::System::Console::ATTACH_PARENT_PROCESS);
    }
    for c in config::load() {
        let mode = if c.mode == config::Mode::Native { "native" } else { "rfb" };
        let user = c.username.map(|u| format!(" {u}")).unwrap_or_default();
        println!("{:16} {}:{} {mode}{user}", c.name, c.host, c.port);
    }
}

fn main() {
    if let Err(message) = run() {
        eprintln!("aktivestream: {message}");
        app::message_box("AktiveStream", &message);
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    // Physical pixels everywhere: the Mac's Retina framebuffer should not be stretched twice.
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }
    // No arguments: show the connection picker, which launches the chosen connection.
    if std::env::args().len() == 1 {
        return picker::run(config::load());
    }
    let args = parse_args()?;
    if args.exit_after.is_some() {
        // Release builds have no console of their own; print diagnostics to the caller's.
        unsafe {
            let _ =
                windows::Win32::System::Console::AttachConsole(windows::Win32::System::Console::ATTACH_PARENT_PROCESS);
        }
    }
    if args.native {
        if !args.extra_hosts.is_empty() {
            return Err("multi-session is RFB only; open native connections one at a time".into());
        }
        if args.tunnel.is_some() {
            return Err(
                "the native path is QUIC/UDP and cannot use an SSH tunnel; use a Tailscale address directly".into()
            );
        }
        return native::run(&args);
    }

    let target = format!("AktiveStream/{}", args.host);
    if args.forget {
        creds::delete(&target);
    }

    // Optional SSH tunnel for the VNC path; kept alive for the whole session.
    let _tunnel;
    let (conn_host, conn_port) = if let Some(spec) = &args.tunnel {
        let active = tunnel::open(spec, &args.host, args.port)?;
        let local_port = active.local_port;
        _tunnel = Some(active);
        ("127.0.0.1".to_string(), local_port)
    } else {
        _tunnel = None;
        (args.host.clone(), args.port)
    };

    // Connect the first target, capturing the login (user, password) to reuse for extra sessions.
    let (session, username, password) = if let Some(var) = &args.password_env {
        let password = std::env::var(var).map_err(|_| format!("environment variable {var} is not set"))?;
        let options = ConnectOptions::new(conn_host.clone(), conn_port, args.user.clone(), password.clone());
        let session = aktivestream_rfb::connect(&options)
            .map_err(|e| format!("Could not connect to {}:{}\n\n{e}", args.host, args.port))?;
        (session, args.user.clone(), password)
    } else {
        let mut saved = creds::read(&target);
        let mut auth_error = 0;
        loop {
            let (login, save) = match saved.take() {
                Some(login) => (login, false),
                None => match creds::prompt(&target, &args.host, args.user.as_deref(), auth_error)? {
                    Some(answer) => answer,
                    None => return Ok(()),
                },
            };
            let username = (!login.user.is_empty()).then(|| login.user.clone());
            let options = ConnectOptions::new(conn_host.clone(), conn_port, username.clone(), login.password.clone());
            match aktivestream_rfb::connect(&options) {
                Ok(session) => {
                    if save {
                        creds::write(&target, &login.user, &login.password);
                    }
                    break (session, username, login.password);
                }
                Err(RfbError::AuthFailed(_) | RfbError::UsernameRequired(_)) => auth_error = ERROR_LOGON_FAILURE.0,
                Err(e) => return Err(format!("Could not connect to {}:{}\n\n{e}", args.host, args.port)),
            }
        }
    };

    if args.extra_hosts.is_empty() {
        return app::run(&args, session);
    }

    // Multi-session: connect each extra target (reusing the first login) and open them together.
    // The tunnels must outlive the window, so keep them in scope for the whole call.
    let mut _extra_tunnels = Vec::new();
    let mut sessions = vec![(args.host.clone(), session)];
    for extra in &args.extra_hosts {
        let (h, p, tun) = resolve_rfb_target(extra)?;
        let (eh, ep) = if let Some(spec) = &tun {
            let active = tunnel::open(spec, &h, p)?;
            let local_port = active.local_port;
            _extra_tunnels.push(active);
            ("127.0.0.1".to_string(), local_port)
        } else {
            (h, p)
        };
        let options = ConnectOptions::new(eh, ep, username.clone(), password.clone());
        let session =
            aktivestream_rfb::connect(&options).map_err(|e| format!("Could not connect to {extra}\n\n{e}"))?;
        sessions.push((extra.clone(), session));
    }
    app::run_multi(&args, sessions)
}

/// Resolves an extra RFB target (a saved name or `host[:port]`) to (host, port, tunnel). Rejects a
/// native saved connection, since multi-session is RFB only.
fn resolve_rfb_target(target: &str) -> Result<(String, u16, Option<String>), String> {
    match config::find(target) {
        Some(c) if c.mode == config::Mode::Native => {
            Err(format!("{target} is a native connection; multi-session is RFB only"))
        }
        Some(c) => Ok((c.host, c.port, c.tunnel)),
        None => match target.rsplit_once(':') {
            Some((h, p)) if !h.contains(':') => {
                Ok((h.to_string(), p.parse().map_err(|_| format!("bad port in {target}"))?, None))
            }
            _ => Ok((target.to_string(), 5900, None)),
        },
    }
}
