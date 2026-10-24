//! The connection manager: a stateful, keyboard-first phone book drawn to a `0x00RRGGBB` buffer.
//! It owns the list (with type-to-filter search and favorites), an add/edit properties form, and a
//! delete confirmation, and it reads and writes the saved connections through [`crate::config`].
//!
//! Platform-neutral: a client feeds it [`KeyEvent`]s and clicks, composites its [`Manager::draw`]
//! output, and acts on the [`Outcome`] — launching a connection, quitting, or persisting a
//! password to its own secret store (Windows Credential Manager, a keyring, ...). The password
//! itself never lives in the connections file; only the connection metadata does.

use crate::config::{self, Connection, Mode};
use crate::draw;

const PAD: usize = 24;
const TITLE_SCALE: usize = 3;
const ROW_SCALE: usize = 2;
const HINT_SCALE: usize = 1;
/// The clickable toolbar of actions, below the title.
const TOOLBAR_Y: usize = 62;
const BTN_H: usize = 34;
const BTN_SCALE: usize = 2;
const BTN_PAD: usize = 14;
const BTN_GAP: usize = 10;
/// The search / help line, below the toolbar.
const SEARCH_Y: usize = 106;
/// Top edge of the first list row (below the title, toolbar and search line).
const LIST_TOP: usize = 134;
const ROW_H: usize = 44;
/// Top edge of the first form field.
const FORM_TOP: usize = 104;
const FIELD_H: usize = 46;
const LABEL_W: usize = 150;

const BG: u32 = 0x16_16_18;
const SELECTED_BG: u32 = 0x24_34_50;
const ACCENT: u32 = 0x5a_8a_ff;
const TEXT: u32 = 0xf0_f0_f2;
const DIM: u32 = 0x80_80_86;
const SUBTLE: u32 = 0xc8_c8_cc;
const FIELD_BG: u32 = 0x1e_1e_22;
const FIELD_BORDER: u32 = 0x3a_3a_40;
const ERROR: u32 = 0xff_6b_6b;
const STAR: u32 = 0xff_c8_4a;
const BTN_BG: u32 = 0x2a2a32;
const BTN_PRIMARY_BG: u32 = 0x27_43_82;
const DANGER: u32 = 0xb8_55_55;

// Form field indices, in focus order.
const F_NAME: usize = 0;
const F_HOST: usize = 1;
const F_PORT: usize = 2;
const F_MODE: usize = 3;
const F_GENERIC: usize = 4;
const F_USER: usize = 5;
const F_PASSWORD: usize = 6;
const F_TUNNEL: usize = 7;
const F_FAVORITE: usize = 8;
const FIELD_COUNT: usize = 9;

/// A key press delivered to the manager. Printable typing arrives as `Char(c)` with `ctrl=false`;
/// control chords (new, edit, duplicate, save, reveal password) arrive as `Char(c)` with
/// `ctrl=true`; everything else is a named key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyCode {
    Up,
    Down,
    Left,
    Right,
    Enter,
    Escape,
    Tab,
    BackTab,
    Backspace,
    Delete,
    Char(char),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeyEvent {
    pub code: KeyCode,
    pub ctrl: bool,
}

impl KeyEvent {
    pub fn new(code: KeyCode) -> KeyEvent {
        KeyEvent { code, ctrl: false }
    }

    pub fn ctrl(code: KeyCode) -> KeyEvent {
        KeyEvent { code, ctrl: true }
    }
}

/// What the client should do after handing the manager an event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// State changed; repaint.
    Redraw,
    /// Nothing changed; no repaint needed.
    Idle,
    /// Launch this saved connection (by name) and close the manager.
    Connect(String),
    /// Close the manager.
    Quit,
    /// Persist this login for `host` to the client's secret store, then repaint.
    StorePassword { host: String, username: String, password: String },
    /// Forget any stored password for `host` (the connection was deleted), then repaint.
    ForgetPassword { host: String },
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum BtnStyle {
    Normal,
    Primary,
    Danger,
}

/// A clickable toolbar/footer button. Clicking it replays `event` through the state machine, so a
/// button and its keyboard shortcut are one code path.
struct Button {
    x: usize,
    y: usize,
    w: usize,
    h: usize,
    label: &'static str,
    event: KeyEvent,
    style: BtnStyle,
}

/// The in-progress add/edit form. Text fields hold their editable strings; the password starts
/// empty when editing (meaning "unchanged") and only updates the secret store if touched.
#[derive(Debug, Clone)]
struct Form {
    /// The name of the connection being edited; `None` when adding a new one.
    original: Option<String>,
    name: String,
    host: String,
    port: String,
    mode: Mode,
    generic: bool,
    username: String,
    password: String,
    password_touched: bool,
    show_password: bool,
    tunnel: String,
    favorite: bool,
    focus: usize,
    error: Option<String>,
}

impl Form {
    fn blank() -> Form {
        Form {
            original: None,
            name: String::new(),
            host: String::new(),
            port: "5900".to_string(),
            mode: Mode::Rfb,
            generic: false,
            username: String::new(),
            password: String::new(),
            password_touched: false,
            show_password: false,
            tunnel: String::new(),
            favorite: false,
            focus: F_NAME,
            error: None,
        }
    }

    fn from_connection(c: &Connection) -> Form {
        Form {
            original: Some(c.name.clone()),
            name: c.name.clone(),
            host: c.host.clone(),
            port: c.port.to_string(),
            mode: c.mode,
            generic: c.generic,
            username: c.username.clone().unwrap_or_default(),
            password: String::new(),
            password_touched: false,
            show_password: false,
            tunnel: c.tunnel.clone().unwrap_or_default(),
            favorite: c.favorite,
            focus: F_NAME,
            error: None,
        }
    }

    fn text_field_mut(&mut self, idx: usize) -> Option<&mut String> {
        match idx {
            F_NAME => Some(&mut self.name),
            F_HOST => Some(&mut self.host),
            F_PORT => Some(&mut self.port),
            F_USER => Some(&mut self.username),
            F_PASSWORD => Some(&mut self.password),
            F_TUNNEL => Some(&mut self.tunnel),
            _ => None,
        }
    }

    fn toggle(&mut self) {
        match self.focus {
            F_MODE => self.mode = if self.mode == Mode::Rfb { Mode::Native } else { Mode::Rfb },
            F_GENERIC => self.generic = !self.generic,
            F_FAVORITE => self.favorite = !self.favorite,
            _ => {}
        }
    }
}

enum View {
    List { selected: usize, search: String },
    Edit(Form),
    ConfirmDelete { name: String, back: usize },
}

/// The connection manager. Build it with the saved connections, feed it input, draw it, and act on
/// the [`Outcome`].
pub struct Manager {
    connections: Vec<Connection>,
    view: View,
    /// Whether changes are written back to the connections file. Off only in unit tests, so they
    /// never touch the real phone book.
    persistent: bool,
}

impl Manager {
    pub fn new(connections: Vec<Connection>) -> Manager {
        Manager::build(connections, true)
    }

    fn build(mut connections: Vec<Connection>, persistent: bool) -> Manager {
        sort(&mut connections);
        Manager { connections, view: View::List { selected: 0, search: String::new() }, persistent }
    }

    /// Indices of the connections matching `search` (case-insensitive over name and host); the
    /// list is already sorted favorites-first.
    fn visible(&self, search: &str) -> Vec<usize> {
        let q = search.to_lowercase();
        self.connections
            .iter()
            .enumerate()
            .filter(|(_, c)| q.is_empty() || c.name.to_lowercase().contains(&q) || c.host.to_lowercase().contains(&q))
            .map(|(i, _)| i)
            .collect()
    }

    fn persist(&self) {
        if self.persistent {
            let _ = config::save(&self.connections);
        }
    }

    pub fn key(&mut self, ev: KeyEvent) -> Outcome {
        enum Tag {
            List,
            Edit,
            Confirm,
        }
        let tag = match &self.view {
            View::List { .. } => Tag::List,
            View::Edit(_) => Tag::Edit,
            View::ConfirmDelete { .. } => Tag::Confirm,
        };
        match tag {
            Tag::List => self.key_list(ev),
            Tag::Edit => self.key_edit(ev),
            Tag::Confirm => self.key_confirm(ev),
        }
    }

    fn key_list(&mut self, ev: KeyEvent) -> Outcome {
        let (mut selected, mut search) = match &self.view {
            View::List { selected, search } => (*selected, search.clone()),
            _ => return Outcome::Idle,
        };
        let vis = self.visible(&search);
        let outcome = match ev.code {
            KeyCode::Up => {
                if !vis.is_empty() {
                    selected = (selected + vis.len() - 1) % vis.len();
                }
                Outcome::Redraw
            }
            KeyCode::Down => {
                if !vis.is_empty() {
                    selected = (selected + 1) % vis.len();
                }
                Outcome::Redraw
            }
            KeyCode::Enter => match vis.get(selected) {
                Some(&i) => Outcome::Connect(self.connections[i].name.clone()),
                None => Outcome::Idle,
            },
            KeyCode::Escape => {
                if search.is_empty() {
                    Outcome::Quit
                } else {
                    search.clear();
                    selected = 0;
                    Outcome::Redraw
                }
            }
            KeyCode::Backspace => {
                if search.pop().is_some() {
                    selected = 0;
                    Outcome::Redraw
                } else {
                    Outcome::Idle
                }
            }
            KeyCode::Delete => match vis.get(selected) {
                Some(&i) => {
                    let name = self.connections[i].name.clone();
                    self.view = View::ConfirmDelete { name, back: selected };
                    return Outcome::Redraw;
                }
                None => Outcome::Idle,
            },
            KeyCode::Char('n') if ev.ctrl => {
                self.view = View::Edit(Form::blank());
                return Outcome::Redraw;
            }
            KeyCode::Char('e') if ev.ctrl => match vis.get(selected) {
                Some(&i) => {
                    self.view = View::Edit(Form::from_connection(&self.connections[i]));
                    return Outcome::Redraw;
                }
                None => Outcome::Idle,
            },
            KeyCode::Char('d') if ev.ctrl => {
                self.duplicate(&vis, selected);
                return Outcome::Redraw;
            }
            KeyCode::Char('f') if ev.ctrl => {
                if let Some(&i) = vis.get(selected) {
                    self.connections[i].favorite ^= true;
                    let name = self.connections[i].name.clone();
                    sort(&mut self.connections);
                    self.persist();
                    // Keep the cursor on the same connection after the re-sort.
                    let vis = self.visible(&search);
                    selected = vis.iter().position(|&i| self.connections[i].name == name).unwrap_or(selected);
                }
                Outcome::Redraw
            }
            KeyCode::Char(c) if !ev.ctrl => {
                search.push(c);
                selected = 0;
                Outcome::Redraw
            }
            _ => Outcome::Idle,
        };
        // Clamp and write the list state back (unless an arm already switched the view).
        let n = self.visible_count(&search);
        if let View::List { selected: s, search: q } = &mut self.view {
            *s = if n == 0 { 0 } else { selected.min(n - 1) };
            *q = search;
        }
        outcome
    }

    fn visible_count(&self, search: &str) -> usize {
        self.visible(search).len()
    }

    fn duplicate(&mut self, vis: &[usize], selected: usize) {
        let Some(&i) = vis.get(selected) else { return };
        let mut copy = self.connections[i].clone();
        copy.favorite = false;
        copy.name = unique_name(&copy.name, &self.connections);
        self.connections.push(copy);
        sort(&mut self.connections);
        self.persist();
    }

    fn key_edit(&mut self, ev: KeyEvent) -> Outcome {
        // Save and cancel need `&mut self`, so handle them before borrowing the form.
        match ev.code {
            KeyCode::Escape => {
                self.back_to_list(None);
                return Outcome::Redraw;
            }
            KeyCode::Enter => return self.save_form(),
            KeyCode::Char('s') if ev.ctrl => return self.save_form(),
            _ => {}
        }
        let View::Edit(form) = &mut self.view else { return Outcome::Idle };
        match ev.code {
            KeyCode::Up | KeyCode::BackTab => {
                form.focus = (form.focus + FIELD_COUNT - 1) % FIELD_COUNT;
                Outcome::Redraw
            }
            KeyCode::Down | KeyCode::Tab => {
                form.focus = (form.focus + 1) % FIELD_COUNT;
                Outcome::Redraw
            }
            KeyCode::Left | KeyCode::Right if matches!(form.focus, F_MODE | F_GENERIC | F_FAVORITE) => {
                form.toggle();
                Outcome::Redraw
            }
            KeyCode::Char('p') if ev.ctrl && form.focus == F_PASSWORD => {
                form.show_password = !form.show_password;
                Outcome::Redraw
            }
            KeyCode::Char(' ') if matches!(form.focus, F_MODE | F_GENERIC | F_FAVORITE) => {
                form.toggle();
                Outcome::Redraw
            }
            KeyCode::Char(c) if !ev.ctrl => {
                if form.focus == F_PORT && (!c.is_ascii_digit() || form.port.len() >= 5) {
                    return Outcome::Idle;
                }
                let is_password = form.focus == F_PASSWORD;
                if let Some(field) = form.text_field_mut(form.focus) {
                    field.push(c);
                    if is_password {
                        form.password_touched = true;
                    }
                    Outcome::Redraw
                } else {
                    Outcome::Idle
                }
            }
            KeyCode::Backspace => {
                let is_password = form.focus == F_PASSWORD;
                if let Some(field) = form.text_field_mut(form.focus)
                    && field.pop().is_some()
                {
                    if is_password {
                        form.password_touched = true;
                    }
                    return Outcome::Redraw;
                }
                Outcome::Idle
            }
            _ => Outcome::Idle,
        }
    }

    fn save_form(&mut self) -> Outcome {
        let View::Edit(form) = &mut self.view else { return Outcome::Idle };
        let name = form.name.trim().to_string();
        let host = form.host.trim().to_string();
        if name.is_empty() {
            form.error = Some("Name is required".into());
            return Outcome::Redraw;
        }
        if host.is_empty() {
            form.error = Some("Host is required".into());
            return Outcome::Redraw;
        }
        let port: u16 = match form.port.trim().parse() {
            Ok(p) if p > 0 => p,
            _ => {
                form.error = Some("Port must be 1-65535".into());
                return Outcome::Redraw;
            }
        };
        let renamed_from = form.original.clone().filter(|old| *old != name);
        if self.connections.iter().any(|c| c.name == name && form.original.as_deref() != Some(c.name.as_str())) {
            form.error = Some("A connection with that name already exists".into());
            return Outcome::Redraw;
        }
        let conn = Connection {
            name: name.clone(),
            host: host.clone(),
            port,
            mode: form.mode,
            username: non_empty(&form.username),
            tunnel: non_empty(&form.tunnel),
            generic: form.mode == Mode::Rfb && form.generic,
            favorite: form.favorite,
        };
        let password = form.password_touched.then(|| form.password.clone());
        let form_username = form.username.trim().to_string();

        if let Some(old) = &renamed_from {
            self.connections.retain(|c| &c.name != old);
        }
        match self.connections.iter_mut().find(|c| c.name == conn.name) {
            Some(existing) => *existing = conn.clone(),
            None => self.connections.push(conn.clone()),
        }
        sort(&mut self.connections);
        self.persist();
        self.back_to_list(Some(&conn.name));
        match password {
            Some(password) => Outcome::StorePassword { host, username: form_username, password },
            None => Outcome::Redraw,
        }
    }

    fn key_confirm(&mut self, ev: KeyEvent) -> Outcome {
        let (name, back) = match &self.view {
            View::ConfirmDelete { name, back } => (name.clone(), *back),
            _ => return Outcome::Idle,
        };
        match ev.code {
            KeyCode::Enter | KeyCode::Char('y') => {
                let host = self.connections.iter().find(|c| c.name == name).map(|c| c.host.clone());
                self.connections.retain(|c| c.name != name);
                self.persist();
                self.back_to_list_at(back);
                match host {
                    Some(host) => Outcome::ForgetPassword { host },
                    None => Outcome::Redraw,
                }
            }
            KeyCode::Escape | KeyCode::Char('n') => {
                self.back_to_list_at(back);
                Outcome::Redraw
            }
            _ => Outcome::Idle,
        }
    }

    /// Returns to the list, selecting `focus_name` if given, else the first row.
    fn back_to_list(&mut self, focus_name: Option<&str>) {
        let search = String::new();
        let selected = focus_name
            .and_then(|name| self.visible(&search).iter().position(|&i| self.connections[i].name == name))
            .unwrap_or(0);
        self.view = View::List { selected, search };
    }

    fn back_to_list_at(&mut self, idx: usize) {
        let n = self.visible_count("");
        let selected = if n == 0 { 0 } else { idx.min(n - 1) };
        self.view = View::List { selected, search: String::new() };
    }

    /// Buttons for the current view: the list toolbar, or the form / confirm footer. Clicking one
    /// replays its `event`, so a button and its shortcut share the state machine.
    fn buttons(&self, dh: usize) -> Vec<Button> {
        use KeyCode::{Char, Delete, Enter, Escape};
        let mut out = Vec::new();
        let mut place = |x: &mut usize, y: usize, label: &'static str, event: KeyEvent, style: BtnStyle| {
            let w = draw::text_width(label, BTN_SCALE) + 2 * BTN_PAD;
            out.push(Button { x: *x, y, w, h: BTN_H, label, event, style });
            *x += w + BTN_GAP;
        };
        match &self.view {
            View::List { .. } => {
                let mut x = PAD;
                place(&mut x, TOOLBAR_Y, "New", KeyEvent::ctrl(Char('n')), BtnStyle::Primary);
                place(&mut x, TOOLBAR_Y, "Edit", KeyEvent::ctrl(Char('e')), BtnStyle::Normal);
                place(&mut x, TOOLBAR_Y, "Duplicate", KeyEvent::ctrl(Char('d')), BtnStyle::Normal);
                place(&mut x, TOOLBAR_Y, "Favorite", KeyEvent::ctrl(Char('f')), BtnStyle::Normal);
                place(&mut x, TOOLBAR_Y, "Delete", KeyEvent::new(Delete), BtnStyle::Danger);
            }
            View::Edit(_) => {
                let y = dh.saturating_sub(BTN_H + 16);
                let mut x = PAD;
                place(&mut x, y, "Save", KeyEvent::new(Enter), BtnStyle::Primary);
                place(&mut x, y, "Cancel", KeyEvent::new(Escape), BtnStyle::Normal);
            }
            View::ConfirmDelete { .. } => {
                let y = LIST_TOP + 48;
                let mut x = PAD;
                place(&mut x, y, "Delete", KeyEvent::new(Enter), BtnStyle::Danger);
                place(&mut x, y, "Cancel", KeyEvent::new(Escape), BtnStyle::Normal);
            }
        }
        out
    }

    fn render_buttons(buf: &mut [u32], dw: usize, buttons: &[Button]) {
        for b in buttons {
            let (bg, border) = match b.style {
                BtnStyle::Primary => (BTN_PRIMARY_BG, ACCENT),
                BtnStyle::Normal => (BTN_BG, FIELD_BORDER),
                BtnStyle::Danger => (BTN_BG, DANGER),
            };
            draw::fill_rect(buf, dw, b.x, b.y, b.w, b.h, bg);
            draw::rect_outline(buf, dw, b.x, b.y, b.w, b.h, border);
            let label_color = if matches!(b.style, BtnStyle::Danger) { DANGER } else { TEXT };
            let ty = b.y + (b.h - 8 * BTN_SCALE) / 2;
            draw::text(buf, dw, b.x + BTN_PAD, ty, b.label, label_color, b.x + b.w, BTN_SCALE);
        }
    }

    /// Highlights the list row under the cursor (no action), so a following toolbar-button click
    /// targets it. Returns whether the selection moved.
    pub fn hover(&mut self, x: i32, y: i32, dh: usize) -> Outcome {
        let search = match &self.view {
            View::List { search, .. } => search.clone(),
            _ => return Outcome::Idle,
        };
        if x < 0 || y < LIST_TOP as i32 {
            return Outcome::Idle;
        }
        let row = (y as usize - LIST_TOP) / ROW_H;
        let vis = self.visible(&search);
        if row >= vis.len() || LIST_TOP + (row + 1) * ROW_H > dh {
            return Outcome::Idle;
        }
        if let View::List { selected, .. } = &mut self.view
            && *selected != row
        {
            *selected = row;
            return Outcome::Redraw;
        }
        Outcome::Idle
    }

    /// A click: a toolbar/footer button (replayed as its key event), or a list row (connect).
    pub fn click(&mut self, x: i32, y: i32, dh: usize) -> Outcome {
        if x >= 0 && y >= 0 {
            let (cx, cy) = (x as usize, y as usize);
            if let Some(event) = self
                .buttons(dh)
                .into_iter()
                .find(|b| cx >= b.x && cx < b.x + b.w && cy >= b.y && cy < b.y + b.h)
                .map(|b| b.event)
            {
                return self.key(event);
            }
        }
        let search = match &self.view {
            View::List { search, .. } => search.clone(),
            _ => return Outcome::Idle,
        };
        if x < 0 || y < LIST_TOP as i32 {
            return Outcome::Idle;
        }
        let row = (y as usize - LIST_TOP) / ROW_H;
        let vis = self.visible(&search);
        if row >= vis.len() || LIST_TOP + (row + 1) * ROW_H > dh {
            return Outcome::Idle;
        }
        if let View::List { selected, .. } = &mut self.view {
            *selected = row;
        }
        Outcome::Connect(self.connections[vis[row]].name.clone())
    }

    pub fn draw(&self, buf: &mut [u32], dw: usize, dh: usize) {
        draw::fill_rect(buf, dw, 0, 0, dw, dh, BG);
        match &self.view {
            View::List { selected, search } => self.draw_list(buf, dw, dh, *selected, search),
            View::Edit(form) => self.draw_form(buf, dw, dh, form),
            View::ConfirmDelete { name, .. } => self.draw_confirm(buf, dw, dh, name),
        }
        let buttons = self.buttons(dh);
        Self::render_buttons(buf, dw, &buttons);
    }

    fn draw_list(&self, buf: &mut [u32], dw: usize, dh: usize, selected: usize, search: &str) {
        let right = dw.saturating_sub(PAD);
        draw::text(buf, dw, PAD, PAD, "AktiveStream", TEXT, right, TITLE_SCALE);
        let search_line = if search.is_empty() {
            "Type to search.   Click a row to connect.   Buttons act on the highlighted row.".to_string()
        } else {
            format!("Search: {search}    (Backspace clears)")
        };
        let search_color = if search.is_empty() { DIM } else { ACCENT };
        draw::text(buf, dw, PAD, SEARCH_Y, &search_line, search_color, right, HINT_SCALE);

        let vis = self.visible(search);
        if vis.is_empty() {
            let msg = if self.connections.is_empty() {
                "No saved connections. Ctrl+N to add one."
            } else {
                "No matches. Backspace to clear the search."
            };
            draw::text(buf, dw, PAD, LIST_TOP, msg, DIM, right, ROW_SCALE);
            return;
        }
        for (row, &i) in vis.iter().enumerate() {
            let ry = LIST_TOP + row * ROW_H;
            if ry + ROW_H > dh {
                break;
            }
            let c = &self.connections[i];
            if row == selected {
                draw::fill_rect(buf, dw, 0, ry, dw, ROW_H, SELECTED_BG);
                draw::fill_rect(buf, dw, 0, ry, 5, ROW_H, ACCENT);
            }
            let ty = ry + ROW_H.saturating_sub(8 * ROW_SCALE) / 2;
            let mut name_x = PAD;
            if c.favorite {
                draw::text(buf, dw, name_x, ty, "*", STAR, right, ROW_SCALE);
                name_x += draw::text_width("* ", ROW_SCALE);
            }
            let hostport = format!("{}:{}", c.host, c.port);
            let hp_x = right.saturating_sub(draw::text_width(&hostport, ROW_SCALE));
            let tag = match (c.mode, c.generic) {
                (Mode::Native, _) => "native",
                (Mode::Rfb, true) => "vnc-pc",
                (Mode::Rfb, false) => "vnc",
            };
            let tag_x = hp_x.saturating_sub(16).saturating_sub(draw::text_width(tag, HINT_SCALE));
            let name_color = if row == selected { TEXT } else { SUBTLE };
            draw::text(buf, dw, name_x, ty, &c.name, name_color, tag_x.saturating_sub(12), ROW_SCALE);
            draw::text(buf, dw, tag_x, ry + ROW_H.saturating_sub(8) / 2, tag, 0x7a_8a_aa, hp_x, HINT_SCALE);
            draw::text(buf, dw, hp_x, ty, &hostport, SUBTLE, right, ROW_SCALE);
        }
    }

    fn draw_form(&self, buf: &mut [u32], dw: usize, dh: usize, form: &Form) {
        let right = dw.saturating_sub(PAD);
        let title = if form.original.is_some() { "Edit connection" } else { "New connection" };
        draw::text(buf, dw, PAD, PAD, title, TEXT, right, TITLE_SCALE);

        let field_x = PAD + LABEL_W;
        let field_w = right.saturating_sub(field_x);
        for idx in 0..FIELD_COUNT {
            let fy = FORM_TOP + idx * FIELD_H;
            if fy + FIELD_H > dh {
                break;
            }
            let focused = idx == form.focus;
            let (label, value, is_toggle) = self.field_view(form, idx);
            let label_color = if focused { TEXT } else { DIM };
            let ly = fy + (28 - 8 * ROW_SCALE) / 2;
            draw::text(buf, dw, PAD, ly, label, label_color, field_x.saturating_sub(8), ROW_SCALE);

            let box_h = 30;
            draw::fill_rect(buf, dw, field_x, fy, field_w, box_h, FIELD_BG);
            draw::rect_outline(buf, dw, field_x, fy, field_w, box_h, if focused { ACCENT } else { FIELD_BORDER });
            let tx = field_x + 8;
            let ty = fy + (box_h - 8 * ROW_SCALE) / 2;
            let value_color = if is_toggle { ACCENT } else { TEXT };
            let shown_w = draw::text(buf, dw, tx, ty, &value, value_color, right.saturating_sub(8), ROW_SCALE);
            if focused && !is_toggle {
                // A caret just past the text.
                draw::fill_rect(buf, dw, tx + shown_w + 1, fy + 6, 2, box_h - 12, ACCENT);
            }
        }

        let hint_y = (FORM_TOP + FIELD_COUNT * FIELD_H + 6).min(dh.saturating_sub(40));
        if let Some(err) = &form.error {
            draw::text(buf, dw, PAD, hint_y, err, ERROR, right, ROW_SCALE);
        }
        let hint = "Tab or arrows move between fields    Space toggles    Ctrl+P shows the password";
        draw::text(buf, dw, PAD, hint_y + 24, hint, DIM, right, HINT_SCALE);
    }

    /// The label, display value, and whether the field is a toggle, for drawing.
    fn field_view(&self, form: &Form, idx: usize) -> (&'static str, String, bool) {
        match idx {
            F_NAME => ("Name", form.name.clone(), false),
            F_HOST => ("Host", form.host.clone(), false),
            F_PORT => ("Port", form.port.clone(), false),
            F_MODE => ("Mode", if form.mode == Mode::Native { "Native (QUIC)" } else { "VNC" }.to_string(), true),
            F_GENERIC => (
                "Keyboard",
                if form.mode == Mode::Native {
                    "Mac (native)".to_string()
                } else if form.generic {
                    "PC / Linux".to_string()
                } else {
                    "Mac".to_string()
                },
                true,
            ),
            F_USER => ("Username", form.username.clone(), false),
            F_PASSWORD => ("Password", self.password_display(form), false),
            F_TUNNEL => ("Tunnel", form.tunnel.clone(), false),
            F_FAVORITE => ("Favorite", if form.favorite { "Yes".to_string() } else { "No".to_string() }, true),
            _ => ("", String::new(), false),
        }
    }

    fn password_display(&self, form: &Form) -> String {
        if !form.password_touched && form.original.is_some() && form.password.is_empty() {
            return "(unchanged)".to_string();
        }
        if form.show_password { form.password.clone() } else { "*".repeat(form.password.chars().count()) }
    }

    fn draw_confirm(&self, buf: &mut [u32], dw: usize, dh: usize, name: &str) {
        let right = dw.saturating_sub(PAD);
        draw::text(buf, dw, PAD, PAD, "Delete connection", TEXT, right, TITLE_SCALE);
        let line = format!("Delete \"{name}\" from the phone book?");
        draw::text(buf, dw, PAD, LIST_TOP, &line, SUBTLE, right, ROW_SCALE);
        let _ = dh;
        // The Delete / Cancel buttons are drawn by `draw`; note the keyboard equivalents below them.
        draw::text(
            buf,
            dw,
            PAD,
            LIST_TOP + 48 + BTN_H + 14,
            "or press Enter to delete, Esc to cancel",
            DIM,
            right,
            HINT_SCALE,
        );
    }
}

/// Sorts favorites to the top, keeping insertion order within each group (stable).
fn sort(connections: &mut [Connection]) {
    connections.sort_by_key(|c| !c.favorite);
}

fn non_empty(s: &str) -> Option<String> {
    let t = s.trim();
    (!t.is_empty()).then(|| t.to_string())
}

/// A name not already used by any connection, appending `-copy`, `-copy2`, ... as needed.
fn unique_name(base: &str, connections: &[Connection]) -> String {
    let taken = |n: &str| connections.iter().any(|c| c.name == n);
    let first = format!("{base}-copy");
    if !taken(&first) {
        return first;
    }
    (2..).map(|i| format!("{base}-copy{i}")).find(|n| !taken(n)).unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn conn(name: &str, favorite: bool) -> Connection {
        Connection { name: name.into(), host: format!("{name}.local"), port: 5900, favorite, ..Default::default() }
    }

    fn mgr() -> Manager {
        // Non-persistent so the tests never write to the real connections file.
        Manager::build(vec![conn("air", false), conn("studio", true), conn("pi", false)], false)
    }

    fn empty() -> Manager {
        Manager::build(vec![], false)
    }

    fn press(m: &mut Manager, code: KeyCode) -> Outcome {
        m.key(KeyEvent::new(code))
    }

    fn ctrl(m: &mut Manager, c: char) -> Outcome {
        m.key(KeyEvent::ctrl(KeyCode::Char(c)))
    }

    fn typed(m: &mut Manager, text: &str) {
        for c in text.chars() {
            m.key(KeyEvent::new(KeyCode::Char(c)));
        }
    }

    fn selected_name(m: &Manager) -> Option<String> {
        match &m.view {
            View::List { selected, search } => m.visible(search).get(*selected).map(|&i| m.connections[i].name.clone()),
            _ => None,
        }
    }

    #[test]
    fn favorites_sort_to_the_top() {
        let m = mgr();
        // studio is the only favorite, so it leads; the rest keep their order.
        assert_eq!(m.connections.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(), ["studio", "air", "pi"]);
    }

    #[test]
    fn enter_connects_the_selected_row() {
        let mut m = mgr();
        assert_eq!(press(&mut m, KeyCode::Down), Outcome::Redraw);
        assert_eq!(press(&mut m, KeyCode::Enter), Outcome::Connect("air".into()));
    }

    #[test]
    fn typing_filters_and_escape_clears() {
        let mut m = mgr();
        typed(&mut m, "pi");
        assert_eq!(selected_name(&m).as_deref(), Some("pi"));
        assert_eq!(press(&mut m, KeyCode::Enter), Outcome::Connect("pi".into()));
        // Escape clears the search rather than quitting while a filter is active.
        assert_eq!(press(&mut m, KeyCode::Escape), Outcome::Redraw);
        assert_eq!(press(&mut m, KeyCode::Escape), Outcome::Quit);
    }

    #[test]
    fn ctrl_f_toggles_favorite_and_keeps_the_cursor() {
        let mut m = mgr();
        typed(&mut m, "pi");
        assert_eq!(ctrl(&mut m, 'f'), Outcome::Redraw);
        assert!(m.connections.iter().find(|c| c.name == "pi").unwrap().favorite);
        // Still pointing at pi after the re-sort.
        assert_eq!(selected_name(&m).as_deref(), Some("pi"));
    }

    #[test]
    fn new_connection_form_saves_and_connects() {
        let mut m = empty();
        assert_eq!(ctrl(&mut m, 'n'), Outcome::Redraw);
        typed(&mut m, "box"); // name
        press(&mut m, KeyCode::Down);
        typed(&mut m, "10.0.0.9"); // host
        press(&mut m, KeyCode::Down);
        for _ in 0..4 {
            press(&mut m, KeyCode::Backspace); // clear the default "5900"
        }
        typed(&mut m, "x5901"); // port: 'x' is rejected, digits kept
        let saved = press(&mut m, KeyCode::Enter);
        assert_eq!(saved, Outcome::Redraw, "no password entered, so just a redraw");
        assert_eq!(m.connections.len(), 1);
        let c = &m.connections[0];
        assert_eq!((c.name.as_str(), c.host.as_str(), c.port), ("box", "10.0.0.9", 5901));
    }

    #[test]
    fn password_entry_is_returned_for_the_client_to_store() {
        let mut m = empty();
        ctrl(&mut m, 'n');
        typed(&mut m, "box");
        press(&mut m, KeyCode::Down);
        typed(&mut m, "h");
        // Jump to the password field and type a secret.
        for _ in 0..F_PASSWORD - F_HOST {
            press(&mut m, KeyCode::Down);
        }
        typed(&mut m, "hunter2");
        match press(&mut m, KeyCode::Enter) {
            Outcome::StorePassword { host, password, .. } => {
                assert_eq!((host.as_str(), password.as_str()), ("h", "hunter2"));
            }
            other => panic!("expected StorePassword, got {other:?}"),
        }
    }

    #[test]
    fn edit_rename_removes_the_old_entry() {
        let mut m = mgr();
        // Select "air" (row 1 after studio) and edit it.
        press(&mut m, KeyCode::Down);
        assert_eq!(selected_name(&m).as_deref(), Some("air"));
        ctrl(&mut m, 'e');
        // Clear the name and retype.
        for _ in 0.."air".len() {
            press(&mut m, KeyCode::Backspace);
        }
        typed(&mut m, "laptop");
        press(&mut m, KeyCode::Enter);
        let names: Vec<_> = m.connections.iter().map(|c| c.name.clone()).collect();
        assert!(names.contains(&"laptop".to_string()));
        assert!(!names.contains(&"air".to_string()));
    }

    #[test]
    fn delete_asks_then_forgets_the_password() {
        let mut m = mgr();
        typed(&mut m, "pi");
        press(&mut m, KeyCode::Delete);
        // Confirm prompt is up; Y deletes and asks the client to forget the secret.
        match m.key(KeyEvent::new(KeyCode::Char('y'))) {
            Outcome::ForgetPassword { host } => assert_eq!(host, "pi.local"),
            other => panic!("expected ForgetPassword, got {other:?}"),
        }
        assert!(!m.connections.iter().any(|c| c.name == "pi"));
    }

    #[test]
    fn duplicate_makes_a_uniquely_named_copy() {
        let mut m = mgr();
        typed(&mut m, "pi");
        ctrl(&mut m, 'd');
        assert!(m.connections.iter().any(|c| c.name == "pi-copy"));
        assert_eq!(m.connections.iter().filter(|c| c.host == "pi.local").count(), 2);
    }

    #[test]
    fn save_rejects_an_empty_host() {
        let mut m = empty();
        ctrl(&mut m, 'n');
        typed(&mut m, "box"); // name only, no host
        assert_eq!(press(&mut m, KeyCode::Enter), Outcome::Redraw);
        match &m.view {
            View::Edit(form) => assert!(form.error.as_deref().unwrap().contains("Host")),
            _ => panic!("should still be on the form"),
        }
    }

    #[test]
    fn clicking_the_new_toolbar_button_opens_the_form() {
        let mut m = mgr();
        // "New" is the first toolbar button, at (PAD, TOOLBAR_Y).
        assert_eq!(m.click((PAD + 6) as i32, (TOOLBAR_Y + 6) as i32, 680), Outcome::Redraw);
        assert!(matches!(m.view, View::Edit(_)));
    }

    #[test]
    fn hover_highlights_the_row_under_the_cursor() {
        let mut m = mgr(); // sorted: studio (fav), air, pi
        assert_eq!(m.hover(10, (LIST_TOP + ROW_H + 4) as i32, 680), Outcome::Redraw);
        assert_eq!(selected_name(&m).as_deref(), Some("air"));
        // Hovering the same row again does nothing.
        assert_eq!(m.hover(10, (LIST_TOP + ROW_H + 4) as i32, 680), Outcome::Idle);
    }

    #[test]
    fn clicking_a_list_row_connects() {
        let mut m = mgr();
        assert_eq!(m.click(10, (LIST_TOP + 4) as i32, 680), Outcome::Connect("studio".into()));
    }

    #[test]
    fn clicking_delete_button_after_hover_asks_to_delete_that_row() {
        let mut m = mgr();
        m.hover(10, (LIST_TOP + ROW_H + 4) as i32, 680); // select "air"
        // The Delete button is the last toolbar button; click near the right end of the toolbar.
        let del = m.buttons(680).into_iter().find(|b| b.label == "Delete").unwrap();
        assert_eq!(m.click((del.x + 4) as i32, (del.y + 4) as i32, 680), Outcome::Redraw);
        match &m.view {
            View::ConfirmDelete { name, .. } => assert_eq!(name, "air"),
            _ => panic!("expected the delete confirmation"),
        }
    }
}
