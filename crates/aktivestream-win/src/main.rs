//! AktiveStream for Windows: a client for macOS Screen Sharing with a PC-to-Mac
//! keyboard profile.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod clipboard;
mod creds;
mod cursor;
mod diag;
mod native;
mod net;
mod render;
mod view;

use aktivestream_rfb::{ConnectOptions, Error as RfbError};
use windows::Win32::Foundation::ERROR_LOGON_FAILURE;
use windows::Win32::UI::HiDpi::{DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext};

const USAGE: &str = "usage: aktivestream <host>[:port] [--user NAME] [--fullscreen] [--forget]\n\n\
  --user NAME            Mac account name to suggest in the login prompt\n\
  --fullscreen           start in full screen\n\
  --forget               delete the saved login for this host first\n\n\
diagnostics:\n\
  --password-env VAR     take the password from environment variable VAR (with --user);\n\
                         skips the prompt and Credential Manager\n\
  --screenshot PATH      with --exit-after, save the rendered client area as PNG\n\
  --exit-after SECS      close the session after SECS seconds and print statistics\n\
  --view-only            send no keyboard or mouse input (keys still reach --keylog)\n\
  --keylog PATH          log every keyboard-hook event, its routing and keysyms, and clipboard traffic\n\
  --native               connect to the Phase 2 native host (QUIC + HEVC) instead of VNC";

pub struct Args {
    pub host: String,
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
}

fn parse_args() -> Result<Args, String> {
    let mut host = None;
    let mut user = None;
    let mut fullscreen = false;
    let mut forget = false;
    let mut password_env = None;
    let mut screenshot = None;
    let mut exit_after = None;
    let mut view_only = false;
    let mut keylog = None;
    let mut native = false;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--user" => user = Some(args.next().ok_or("--user needs a name")?),
            "--fullscreen" => fullscreen = true,
            "--forget" => forget = true,
            "--view-only" => view_only = true,
            "--native" => native = true,
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
            _ => return Err(format!("unexpected argument {arg}\n\n{USAGE}")),
        }
    }
    let target = host.ok_or_else(|| USAGE.to_string())?;
    let (host, port) = match target.rsplit_once(':') {
        Some((h, p)) if !h.contains(':') => (h.to_string(), p.parse().map_err(|_| format!("bad port in {target}"))?),
        _ => (target, 5900),
    };
    Ok(Args { host, port, user, fullscreen, forget, password_env, screenshot, exit_after, view_only, keylog, native })
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
    let args = parse_args()?;
    if args.exit_after.is_some() {
        // Release builds have no console of their own; print diagnostics to the caller's.
        unsafe {
            let _ =
                windows::Win32::System::Console::AttachConsole(windows::Win32::System::Console::ATTACH_PARENT_PROCESS);
        }
    }
    if args.native {
        return native::run(&args);
    }

    let target = format!("AktiveStream/{}", args.host);
    if args.forget {
        creds::delete(&target);
    }

    if let Some(var) = &args.password_env {
        let password = std::env::var(var).map_err(|_| format!("environment variable {var} is not set"))?;
        let options = ConnectOptions::new(args.host.clone(), args.port, args.user.clone(), password);
        let session = aktivestream_rfb::connect(&options)
            .map_err(|e| format!("Could not connect to {}:{}\n\n{e}", args.host, args.port))?;
        return app::run(&args, session);
    }

    let mut saved = creds::read(&target);
    let mut auth_error = 0;
    let session = loop {
        let (login, save) = match saved.take() {
            Some(login) => (login, false),
            None => match creds::prompt(&target, &args.host, args.user.as_deref(), auth_error)? {
                Some(answer) => answer,
                None => return Ok(()),
            },
        };
        let username = (!login.user.is_empty()).then(|| login.user.clone());
        let options = ConnectOptions::new(args.host.clone(), args.port, username, login.password.clone());
        match aktivestream_rfb::connect(&options) {
            Ok(session) => {
                if save {
                    creds::write(&target, &login.user, &login.password);
                }
                break session;
            }
            Err(RfbError::AuthFailed(_) | RfbError::UsernameRequired(_)) => auth_error = ERROR_LOGON_FAILURE.0,
            Err(e) => return Err(format!("Could not connect to {}:{}\n\n{e}", args.host, args.port)),
        }
    };
    app::run(&args, session)
}
