//! Connection phone book: named saved connections read from a simple text file, so a host can be
//! launched by name (`aktivestream <name>`) instead of typing its address and mode each time.
//! The data layer the future GUI list and multi-session switching build on. Lean on purpose: a
//! whitespace-separated line per connection, no serde/toml dependency.
//!
//! File (`$XDG_CONFIG_HOME/aktivestream/connections`, else `~/.config/...`, else `%APPDATA%\...`):
//!   # name            host            port   mode     [username]
//!   mac               192.168.1.50    35390  native
//!   mac-screenshare   192.168.1.50    5900   rfb      user

use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Mode {
    #[default]
    Rfb,
    Native,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Connection {
    pub name: String,
    pub host: String,
    pub port: u16,
    pub mode: Mode,
    pub username: Option<String>,
    /// Optional SSH tunnel spec `[user@]host[:port]` (RFB only); `tunnel=...` in the file.
    pub tunnel: Option<String>,
    /// A generic PC/Linux VNC server (wayvnc, TigerVNC, ...) rather than a Mac: the PC keyboard
    /// profile and no Apple-only extensions (RFB only); a bare `generic` token in the file.
    pub generic: bool,
    /// Pinned to the top of the connection list; a bare `favorite` token in the file.
    pub favorite: bool,
}

/// Path to the connections file, honouring XDG then `HOME` then (Windows) `APPDATA`.
pub fn config_path() -> Option<PathBuf> {
    if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME")
        && !xdg.is_empty()
    {
        return Some(PathBuf::from(xdg).join("aktivestream").join("connections"));
    }
    if let Ok(home) = std::env::var("HOME")
        && !home.is_empty()
    {
        return Some(PathBuf::from(home).join(".config").join("aktivestream").join("connections"));
    }
    if let Ok(appdata) = std::env::var("APPDATA")
        && !appdata.is_empty()
    {
        return Some(PathBuf::from(appdata).join("aktivestream").join("connections"));
    }
    None
}

/// Loads the saved connections, skipping blank lines, comments, and malformed entries.
pub fn load() -> Vec<Connection> {
    let Some(path) = config_path() else { return Vec::new() };
    let Ok(text) = std::fs::read_to_string(&path) else { return Vec::new() };
    parse(&text)
}

fn parse(text: &str) -> Vec<Connection> {
    text.lines().filter_map(parse_line).collect()
}

fn parse_line(line: &str) -> Option<Connection> {
    let line = line.trim();
    if line.is_empty() || line.starts_with('#') {
        return None;
    }
    let mut fields = line.split_whitespace();
    let name = fields.next()?.to_string();
    let host = fields.next()?.to_string();
    let port: u16 = fields.next()?.parse().ok()?;
    let mode = match fields.next()? {
        "native" => Mode::Native,
        "rfb" => Mode::Rfb,
        _ => return None,
    };
    // Remaining tokens: a bare username, a `tunnel=<spec>` token, and/or a bare `generic` flag,
    // in any order. `generic` is checked before the username fallback so it is never taken as one.
    let mut username = None;
    let mut tunnel = None;
    let mut generic = false;
    let mut favorite = false;
    for token in fields {
        if let Some(spec) = token.strip_prefix("tunnel=") {
            tunnel = Some(spec.to_string());
        } else if token == "generic" {
            generic = true;
        } else if token == "favorite" {
            favorite = true;
        } else if username.is_none() {
            username = Some(token.to_string());
        }
    }
    Some(Connection { name, host, port, mode, username, tunnel, generic, favorite })
}

/// Serializes connections back to the file format: a header comment and one aligned line each.
pub fn serialize(connections: &[Connection]) -> String {
    let mut out =
        String::from("# name            host            port   mode     [username] [tunnel=spec] [generic]\n");
    for c in connections {
        let mode = match c.mode {
            Mode::Native => "native",
            Mode::Rfb => "rfb",
        };
        let mut line = format!("{:<17} {:<15} {:<6} {:<7}", c.name, c.host, c.port, mode);
        if let Some(user) = &c.username {
            line.push_str(&format!(" {user}"));
        }
        if let Some(tunnel) = &c.tunnel {
            line.push_str(&format!(" tunnel={tunnel}"));
        }
        if c.generic {
            line.push_str(" generic");
        }
        if c.favorite {
            line.push_str(" favorite");
        }
        out.push_str(line.trim_end());
        out.push('\n');
    }
    out
}

/// Writes the given connections to the config file, creating the directory if needed.
pub fn save(connections: &[Connection]) -> std::io::Result<()> {
    let path = config_path().ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "no config directory"))?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&path, serialize(connections))
}

/// Adds `connection`, or replaces an existing one with the same name, then saves.
pub fn upsert(connection: Connection) -> std::io::Result<()> {
    let mut all = load();
    match all.iter_mut().find(|c| c.name == connection.name) {
        Some(existing) => *existing = connection,
        None => all.push(connection),
    }
    save(&all)
}

/// Removes the connection with this name, if present. Returns whether one was removed.
pub fn remove(name: &str) -> std::io::Result<bool> {
    let mut all = load();
    let before = all.len();
    all.retain(|c| c.name != name);
    let removed = all.len() != before;
    if removed {
        save(&all)?;
    }
    Ok(removed)
}

/// Finds a saved connection by exact name.
pub fn find(name: &str) -> Option<Connection> {
    load().into_iter().find(|c| c.name == name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_native_and_rfb_lines() {
        let text = "\
# a comment
mac            192.168.1.50   35390  native
mac-share      192.168.1.50   5900   rfb     user
mac-remote     192.168.1.50   5900   rfb     user  tunnel=me@jump:2222
pi             192.168.50.56  5901   rfb     generic favorite

bad line with too few
weird          host           70000  native
unknownmode    host           5900   telnet
";
        let got = parse(text);
        assert_eq!(
            got,
            vec![
                Connection {
                    name: "mac".into(),
                    host: "192.168.1.50".into(),
                    port: 35390,
                    mode: Mode::Native,
                    ..Default::default()
                },
                Connection {
                    name: "mac-share".into(),
                    host: "192.168.1.50".into(),
                    port: 5900,
                    mode: Mode::Rfb,
                    username: Some("user".into()),
                    ..Default::default()
                },
                Connection {
                    name: "mac-remote".into(),
                    host: "192.168.1.50".into(),
                    port: 5900,
                    mode: Mode::Rfb,
                    username: Some("user".into()),
                    tunnel: Some("me@jump:2222".into()),
                    ..Default::default()
                },
                Connection {
                    name: "pi".into(),
                    host: "192.168.50.56".into(),
                    port: 5901,
                    mode: Mode::Rfb,
                    generic: true,
                    favorite: true,
                    ..Default::default()
                },
            ]
        );
    }

    #[test]
    fn empty_and_comment_lines_are_skipped() {
        assert!(parse_line("   ").is_none());
        assert!(parse_line("# comment").is_none());
    }

    #[test]
    fn serialize_round_trips_through_parse() {
        let connections = vec![
            Connection {
                name: "mac".into(),
                host: "192.168.1.50".into(),
                port: 35390,
                mode: Mode::Native,
                ..Default::default()
            },
            Connection {
                name: "pi".into(),
                host: "192.168.50.56".into(),
                port: 5901,
                mode: Mode::Rfb,
                username: Some("jack".into()),
                tunnel: Some("me@jump:2222".into()),
                generic: true,
                favorite: true,
            },
        ];
        // The header is a comment, so parse ignores it; every field survives the round trip.
        assert_eq!(parse(&serialize(&connections)), connections);
    }
}
