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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Rfb,
    Native,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Connection {
    pub name: String,
    pub host: String,
    pub port: u16,
    pub mode: Mode,
    pub username: Option<String>,
    /// Optional SSH tunnel spec `[user@]host[:port]` (RFB only); `tunnel=...` in the file.
    pub tunnel: Option<String>,
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
    // Remaining tokens: a bare username and/or a `tunnel=<spec>` token, in any order.
    let mut username = None;
    let mut tunnel = None;
    for token in fields {
        if let Some(spec) = token.strip_prefix("tunnel=") {
            tunnel = Some(spec.to_string());
        } else if username.is_none() {
            username = Some(token.to_string());
        }
    }
    Some(Connection { name, host, port, mode, username, tunnel })
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
                    username: None,
                    tunnel: None,
                },
                Connection {
                    name: "mac-share".into(),
                    host: "192.168.1.50".into(),
                    port: 5900,
                    mode: Mode::Rfb,
                    username: Some("user".into()),
                    tunnel: None,
                },
                Connection {
                    name: "mac-remote".into(),
                    host: "192.168.1.50".into(),
                    port: 5900,
                    mode: Mode::Rfb,
                    username: Some("user".into()),
                    tunnel: Some("me@jump:2222".into()),
                },
            ]
        );
    }

    #[test]
    fn empty_and_comment_lines_are_skipped() {
        assert!(parse_line("   ").is_none());
        assert!(parse_line("# comment").is_none());
    }
}
