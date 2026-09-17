use std::{
    io::{self, IsTerminal},
    path::PathBuf,
    sync::{atomic::Ordering, mpsc::TryRecvError},
    time::{Duration, Instant},
};

use crossterm::{
    event::{
        self, DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
        Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent,
        MouseEventKind,
    },
    execute,
};
use quadrille::{Progress, Result, SaveJob, Sheet, SortJob, parse_sort};
use ratatui::{
    Frame,
    layout::{Constraint, Flex, Layout, Margin, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Cell, Clear, Paragraph, Row, Table, Tabs, Wrap},
};
use unicode_width::UnicodeWidthStr;

mod cli;

const GRID_BG: Color = Color::Rgb(18, 18, 18);
const GRID_FG: Color = Color::Rgb(238, 238, 238);
const ROW_FG: Color = Color::Rgb(178, 184, 196);
const EDITED_FG: Color = Color::Rgb(255, 221, 87);
const SELECTED_BG: Color = Color::Rgb(255, 215, 64);
const SELECTED_FG: Color = Color::Rgb(17, 24, 39);
const BAR_BG: Color = Color::Rgb(0, 69, 138);
const BAR_FG: Color = Color::Rgb(255, 255, 255);
const PANEL_BG: Color = Color::Rgb(24, 48, 96);
const PANEL_FG: Color = Color::Rgb(255, 255, 255);
const PANEL_ACCENT: Color = Color::Rgb(134, 226, 255);
const PANEL_BORDER: Color = Color::Rgb(118, 196, 255);
const INPUT_BG: Color = Color::Rgb(248, 250, 252);
const INPUT_FG: Color = Color::Rgb(17, 24, 39);

fn main() {
    if let Err(error) = run() {
        eprintln!("qd: {error}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let Some(sheet) = cli::open()? else {
        return Ok(());
    };
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        return Err(
            "The editor needs a terminal. Use --read, --apply or --check for headless commands"
                .into(),
        );
    }
    let mut terminal = ratatui::try_init()?;
    let previous_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = execute!(io::stdout(), DisableMouseCapture, DisableBracketedPaste);
        previous_hook(info);
    }));
    let result = (|| {
        execute!(io::stdout(), EnableBracketedPaste, EnableMouseCapture)?;
        App::new(sheet).run(&mut terminal)
    })();
    let _ = execute!(io::stdout(), DisableMouseCapture, DisableBracketedPaste);
    ratatui::restore();
    result
}

#[derive(Clone, Copy, PartialEq)]
enum Action {
    Edit,
    Save,
    Goto,
    Sort,
    Sheet,
    Quit,
}

struct Prompt {
    action: Action,
    text: String,
    cursor: usize,
    select_all: bool,
    header: bool,
    choices: Vec<String>,
}

impl Prompt {
    fn new(action: Action, text: String) -> Self {
        let cursor = text.len();
        Self {
            action,
            text,
            cursor,
            select_all: matches!(action, Action::Edit | Action::Sort | Action::Sheet),
            header: true,
            choices: Vec::new(),
        }
    }

    fn insert(&mut self, text: &str) {
        if self.select_all {
            self.text.clear();
            self.cursor = 0;
            self.select_all = false;
        }
        self.text.insert_str(self.cursor, text);
        self.cursor += text.len();
    }

    fn key(&mut self, key: KeyEvent) {
        if self.action == Action::Sheet && matches!(key.code, KeyCode::Up | KeyCode::Down) {
            let i = self
                .choices
                .iter()
                .position(|s| s == &self.text)
                .unwrap_or(0);
            let i = if key.code == KeyCode::Up {
                i.saturating_sub(1)
            } else {
                (i + 1).min(self.choices.len().saturating_sub(1))
            };
            if let Some(name) = self.choices.get(i) {
                self.text = name.clone();
                self.cursor = self.text.len();
                self.select_all = true;
            }
            return;
        }
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Char('a') if ctrl => self.select_all = true,
            KeyCode::Char('j') if ctrl && self.action == Action::Edit => self.insert("\n"),
            KeyCode::Char(c) if !ctrl && !key.modifiers.contains(KeyModifiers::ALT) => {
                self.insert(&c.to_string())
            }
            KeyCode::Left => {
                self.select_all = false;
                self.cursor = self.text[..self.cursor]
                    .char_indices()
                    .next_back()
                    .map_or(0, |(i, _)| i);
            }
            KeyCode::Right => {
                self.select_all = false;
                self.cursor += self.text[self.cursor..]
                    .chars()
                    .next()
                    .map_or(0, char::len_utf8);
            }
            KeyCode::Home => {
                self.select_all = false;
                self.cursor = 0;
            }
            KeyCode::End => {
                self.select_all = false;
                self.cursor = self.text.len();
            }
            KeyCode::Backspace | KeyCode::Delete if self.select_all => {
                self.text.clear();
                self.cursor = 0;
                self.select_all = false;
            }
            KeyCode::Backspace if self.cursor > 0 => {
                let before = self.text[..self.cursor]
                    .char_indices()
                    .next_back()
                    .unwrap()
                    .0;
                self.text.drain(before..self.cursor);
                self.cursor = before;
            }
            KeyCode::Delete if self.cursor < self.text.len() => {
                let after =
                    self.cursor + self.text[self.cursor..].chars().next().unwrap().len_utf8();
                self.text.drain(self.cursor..after);
            }
            _ => {}
        }
    }
}

struct App {
    sheet: Sheet,
    rows: Vec<csv::StringRecord>,
    row: u64,
    col: usize,
    top: u64,
    left: usize,
    width: u16,
    loaded: Option<(u64, usize, u64)>,
    prompt: Option<Prompt>,
    save: Option<SaveJob>,
    sorting: Option<SortJob>,
    help: Option<Help>,
    help_limit: u16,
    last_click: Option<(u64, usize, Instant)>,
    unsaved: bool,
    message: String,
    pending_row: Option<u64>,
    saved_workbook: Option<PathBuf>,
}

impl App {
    fn new(sheet: Sheet) -> Self {
        Self {
            sheet,
            rows: Vec::new(),
            row: 0,
            col: 0,
            top: 0,
            left: 0,
            width: 22,
            loaded: None,
            prompt: None,
            save: None,
            sorting: None,
            help: None,
            help_limit: 0,
            last_click: None,
            unsaved: false,
            message: "? / h Help · click a cell · double-click to edit · F6 Sort".into(),
            pending_row: None,
            saved_workbook: None,
        }
    }

    fn run(&mut self, terminal: &mut ratatui::DefaultTerminal) -> Result<()> {
        loop {
            let progress = self.sheet.progress();
            if let Some(job) = &self.save {
                match job.result.try_recv() {
                    Ok(Ok(path)) => {
                        if self.sheet.sheet_name().is_some()
                            && path.extension().is_some_and(|e| {
                                e.to_string_lossy()
                                    .eq_ignore_ascii_case(self.sheet.format())
                            })
                        {
                            self.saved_workbook = Some(path.clone());
                        }
                        self.message = format!("Saved {} — source unchanged", path.display());
                        self.unsaved = self.sheet.sheet_name().is_some()
                            && path
                                .extension()
                                .is_some_and(|e| e.eq_ignore_ascii_case("csv"))
                            && (self.sheet.edit_count() > 0 || self.sheet.sort_order().is_some());
                        self.save = None;
                    }
                    Ok(Err(error)) => {
                        self.message = format!("Save failed: {error}");
                        self.save = None;
                    }
                    Err(TryRecvError::Disconnected) => {
                        self.message = "Save worker stopped unexpectedly".into();
                        self.save = None;
                    }
                    Err(TryRecvError::Empty) => {}
                }
            }
            if let Some(job) = &self.sorting {
                match job.result.try_recv() {
                    Ok(Ok(order)) => {
                        match self.sheet.apply_sort(order) {
                            Ok(()) => {
                                self.row = 0;
                                self.top = 0;
                                self.loaded = None;
                                self.pending_row = None;
                                self.unsaved = true;
                                self.message = if self.sheet.sheet_name().is_some() {
                                    "Sorted view · Save As .csv to export · clear sort for native workbook save".into()
                                } else {
                                    "Sorted · Save As writes this order · F6, clear restores source order".into()
                                };
                            }
                            Err(error) => self.message = error.to_string(),
                        }
                        self.sorting = None;
                    }
                    Ok(Err(error)) => {
                        self.message = error;
                        self.sorting = None;
                    }
                    Err(TryRecvError::Disconnected) => {
                        self.message = "Sort worker stopped unexpectedly".into();
                        self.sorting = None;
                    }
                    Err(TryRecvError::Empty) => {}
                }
            }
            if let Some(target) = self.pending_row {
                if target < progress.rows || progress.done {
                    self.row = target.min(progress.rows.saturating_sub(1));
                    self.pending_row = None;
                    self.message = if target >= progress.rows {
                        "Requested row is beyond the end of the file".into()
                    } else {
                        String::new()
                    };
                }
            }
            let size = terminal.size()?;
            let screen = Rect::new(0, 0, size.width, size.height);
            if let Some(help) = &mut self.help {
                let body = help_layout(screen)[2];
                self.help_limit = if body.width == 0 || body.height == 0 {
                    0
                } else {
                    Paragraph::new(help_lines(help.tab).join("\n"))
                        .wrap(Wrap { trim: false })
                        .line_count(body.width)
                        .saturating_sub(body.height as usize) as u16
                };
                help.scroll = help.scroll.min(self.help_limit);
            }
            let grid = screen_layout(screen)[1].inner(Margin::new(1, 1));
            let height = grid.height.saturating_sub(1).max(1) as usize;
            let columns = grid_columns(grid, self.width);
            if self.row < self.top {
                self.top = self.row;
            }
            if self.row >= self.top + height as u64 {
                self.top = self.row + 1 - height as u64;
            }
            let key = (
                self.top,
                height,
                progress.rows.min(self.top + height as u64),
            );
            if self.loaded != Some(key) {
                match self.sheet.window(self.top, height) {
                    Ok(rows) => {
                        self.rows = rows;
                        self.loaded = Some(key);
                    }
                    Err(error) => {
                        self.message = error.to_string();
                        self.rows.clear();
                        self.loaded = Some(key);
                    }
                }
            }
            let cols = self
                .rows
                .get((self.row - self.top) as usize)
                .map_or(1, |r| r.len())
                .max(1);
            self.col = self.col.min(cols - 1);
            if self.col < self.left {
                self.left = self.col;
            }
            if self.col >= self.left + columns {
                self.left = self.col + 1 - columns;
            }
            terminal.draw(|frame| self.draw(frame, &progress, columns))?;
            if !event::poll(Duration::from_millis(80))? {
                continue;
            }
            match event::read()? {
                Event::Key(key) if key.kind != KeyEventKind::Release => {
                    if self.key(key, height, &progress)? {
                        return Ok(());
                    }
                }
                Event::Mouse(mouse) => {
                    if self.mouse(mouse, screen, height, &progress)? {
                        return Ok(());
                    }
                }
                Event::Paste(text) => {
                    if let Some(prompt) = &mut self.prompt {
                        if prompt.action == Action::Edit {
                            prompt.insert(&text);
                        } else if prompt.action != Action::Quit {
                            prompt.insert(&text.replace(['\r', '\n'], ""));
                        }
                    }
                }
                _ => {}
            }
        }
    }

    fn current(&self) -> Option<&str> {
        let value = self
            .rows
            .get((self.row - self.top) as usize)?
            .get(self.col)?;
        Some(self.sheet.value(self.row, self.col, value))
    }

    fn key(&mut self, key: KeyEvent, height: usize, p: &Progress) -> Result<bool> {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        if let Some(help) = &mut self.help {
            match key.code {
                KeyCode::Esc | KeyCode::F(1) | KeyCode::Char('?' | 'h' | 'q') => self.help = None,
                KeyCode::Right | KeyCode::Tab => {
                    help.tab = (help.tab + 1) % HELP_TITLES.len();
                    help.scroll = 0;
                }
                KeyCode::Left | KeyCode::BackTab => {
                    help.tab = (help.tab + HELP_TITLES.len() - 1) % HELP_TITLES.len();
                    help.scroll = 0;
                }
                KeyCode::Home => {
                    help.tab = 0;
                    help.scroll = 0;
                }
                KeyCode::End => {
                    help.tab = HELP_TITLES.len() - 1;
                    help.scroll = 0;
                }
                KeyCode::Down => help.scroll = (help.scroll + 1).min(self.help_limit),
                KeyCode::PageDown => help.scroll = (help.scroll + 8).min(self.help_limit),
                KeyCode::Up => help.scroll = help.scroll.saturating_sub(1),
                KeyCode::PageUp => help.scroll = help.scroll.saturating_sub(8),
                _ => {}
            }
            return Ok(false);
        }
        if let Some(mut prompt) = self.prompt.take() {
            if key.code == KeyCode::Esc {
                return Ok(false);
            }
            if prompt.action == Action::Quit {
                if matches!(key.code, KeyCode::Char('y' | 'Y')) {
                    return Ok(true);
                }
                if !matches!(key.code, KeyCode::Char('n' | 'N')) {
                    self.prompt = Some(prompt);
                }
                return Ok(false);
            }
            if prompt.action == Action::Sort && key.code == KeyCode::F(2) {
                prompt.header = !prompt.header;
                self.prompt = Some(prompt);
                return Ok(false);
            }
            if key.code == KeyCode::Enter {
                match prompt.action {
                    Action::Edit => {
                        let changed = self.current().is_some_and(|value| value != prompt.text);
                        match self.sheet.set(self.row, self.col, prompt.text) {
                            Ok(()) if changed => {
                                self.unsaved = true;
                                self.saved_workbook = None;
                                self.message = "Cell updated · Ctrl+Z undo".into();
                            }
                            Ok(()) => {}
                            Err(error) => self.message = error.to_string(),
                        }
                    }
                    Action::Save => match self.sheet.save_as(&PathBuf::from(&prompt.text)) {
                        Ok(job) => {
                            self.save = Some(job);
                            self.message = "Saving a snapshot to a new file…".into();
                        }
                        Err(error) => self.message = error.to_string(),
                    },
                    Action::Goto => match prompt.text.trim().parse::<u64>() {
                        Ok(row) if row > 0 => {
                            self.pending_row = Some(row - 1);
                            self.message = format!("Waiting for row {row} to be indexed…");
                        }
                        _ => self.message = "Enter a row number starting at 1".into(),
                    },
                    Action::Sort => {
                        if prompt.text.trim().eq_ignore_ascii_case("clear") {
                            self.sheet.clear_sort();
                            self.loaded = None;
                            self.row = 0;
                            self.top = 0;
                            self.pending_row = None;
                            self.unsaved = true;
                            self.message = "Source order restored; edits retained".into();
                        } else {
                            match parse_sort(&prompt.text)
                                .and_then(|keys| self.sheet.start_sort(keys, prompt.header))
                            {
                                Ok(job) => {
                                    self.sorting = Some(job);
                                    self.message = "Sorting in background · Esc cancels".into();
                                }
                                Err(error) => self.message = error.to_string(),
                            }
                        }
                    }
                    Action::Sheet => {
                        let text = prompt.text.as_str();
                        let name = prompt
                            .choices
                            .iter()
                            .find(|name| name.as_str() == text)
                            .or_else(|| {
                                text.trim()
                                    .parse::<usize>()
                                    .ok()
                                    .and_then(|n| n.checked_sub(1))
                                    .and_then(|n| prompt.choices.get(n))
                            });
                        if let Some(name) = name {
                            let source = self.saved_workbook.as_ref().unwrap_or(&self.sheet.path);
                            match Sheet::open_sheet(source, b',', Some(name)) {
                                Ok(sheet) => {
                                    *self = Self::new(sheet);
                                    self.message = "Sheet opened · w selects another sheet".into();
                                }
                                Err(error) => self.message = error.to_string(),
                            }
                        } else {
                            self.message = "Choose a sheet name or its number".into();
                        }
                    }
                    Action::Quit => unreachable!(),
                }
            } else {
                prompt.key(key);
                self.prompt = Some(prompt);
            }
            return Ok(false);
        }
        match key.code {
            KeyCode::Char('?' | 'h') | KeyCode::F(1) => self.help = Some(Help::default()),
            KeyCode::Char('s') if !ctrl => self.begin_sort(),
            KeyCode::F(6) => self.begin_sort(),
            KeyCode::Char('w') if !ctrl => self.begin_sheet(),
            KeyCode::Esc if self.sorting.is_some() => {
                self.sorting
                    .as_ref()
                    .unwrap()
                    .cancel
                    .store(true, Ordering::Relaxed);
                self.message = "Cancelling sort…".into();
            }
            KeyCode::F(10) => {
                return self.key(
                    KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE),
                    height,
                    p,
                );
            }
            KeyCode::Char('q') | KeyCode::Char('c') if key.code == KeyCode::Char('q') || ctrl => {
                if self.save.is_some() || self.sorting.is_some() {
                    self.message = "Wait for the operation to finish; Esc cancels sorting".into();
                } else if self.unsaved {
                    self.prompt = Some(Prompt::new(Action::Quit, String::new()));
                } else {
                    return Ok(true);
                }
            }
            KeyCode::Char('s') if ctrl => self.begin_save(),
            KeyCode::F(4) => self.begin_save(),
            KeyCode::Char('g') if ctrl => {
                self.prompt = Some(Prompt::new(Action::Goto, String::new()))
            }
            KeyCode::Char('z') if ctrl && self.save.is_none() && self.sorting.is_none() => {
                if self.sheet.undo() {
                    self.unsaved = true;
                    self.saved_workbook = None;
                    self.message = "Undone".into();
                } else {
                    self.message = "Nothing to undo".into();
                }
            }
            KeyCode::Enter | KeyCode::F(2) if self.save.is_none() && self.sorting.is_none() => {
                if let Some(value) = self.current() {
                    self.prompt = Some(Prompt::new(Action::Edit, value.to_owned()));
                }
            }
            KeyCode::Up => {
                self.pending_row = None;
                self.row = self.row.saturating_sub(1);
            }
            KeyCode::Down => {
                self.pending_row = None;
                self.row = self.row.saturating_add(1).min(p.rows.saturating_sub(1));
            }
            KeyCode::PageUp => {
                self.pending_row = None;
                self.row = self.row.saturating_sub(height as u64);
            }
            KeyCode::PageDown => {
                self.pending_row = None;
                self.row = self
                    .row
                    .saturating_add(height as u64)
                    .min(p.rows.saturating_sub(1));
            }
            KeyCode::Left => self.col = self.col.saturating_sub(1),
            KeyCode::Right | KeyCode::Tab => self.col = self.col.saturating_add(1),
            KeyCode::BackTab => self.col = self.col.saturating_sub(1),
            KeyCode::Home if ctrl => {
                self.pending_row = None;
                self.row = 0;
                self.col = 0;
            }
            KeyCode::End if ctrl => {
                self.pending_row = None;
                self.row = p.rows.saturating_sub(1);
            }
            KeyCode::Home => self.col = 0,
            KeyCode::End => {
                self.col = self
                    .rows
                    .get((self.row - self.top) as usize)
                    .map_or(0, |r| r.len().saturating_sub(1))
            }
            KeyCode::Char('+') | KeyCode::Char('=') => self.width = (self.width + 2).min(80),
            KeyCode::Char('-') => self.width = self.width.saturating_sub(2).max(8),
            _ => {}
        }
        Ok(false)
    }

    fn begin_sort(&mut self) {
        if self.save.is_some() || self.sorting.is_some() {
            return;
        }
        let mut prompt = Prompt::new(Action::Sort, column_name(self.col));
        if let Some(order) = self.sheet.sort_order() {
            prompt.header = order.header;
        }
        self.prompt = Some(prompt);
    }

    fn begin_sheet(&mut self) {
        if self.sheet.sheet_name().is_none() {
            self.message = "CSV files have one sheet".into();
            return;
        }
        if self.save.is_some() || self.sorting.is_some() {
            return;
        }
        if self.sheet.edit_count() > 0 && (self.unsaved || self.saved_workbook.is_none()) {
            self.message = "Save edits to a native workbook before switching sheets (CSV export saves only values)".into();
            return;
        }
        let mut prompt = Prompt::new(Action::Sheet, self.sheet.sheet_name().unwrap().to_owned());
        prompt.choices = self.sheet.workbook_sheet_names().to_vec();
        self.prompt = Some(prompt);
    }

    fn mouse(
        &mut self,
        mouse: MouseEvent,
        screen: Rect,
        height: usize,
        p: &Progress,
    ) -> Result<bool> {
        if self.help.is_some() {
            let key = match mouse.kind {
                MouseEventKind::ScrollDown => Some(KeyCode::Down),
                MouseEventKind::ScrollUp => Some(KeyCode::Up),
                MouseEventKind::Down(MouseButton::Right) => Some(KeyCode::Esc),
                _ => None,
            };
            if let Some(key) = key {
                self.key(KeyEvent::new(key, KeyModifiers::NONE), height, p)?;
            }
            return Ok(false);
        }
        if self.prompt.is_some() {
            return Ok(false);
        }
        let areas = screen_layout(screen);
        if mouse.kind == MouseEventKind::Down(MouseButton::Left)
            && areas[5].contains((mouse.column, mouse.row).into())
        {
            if let Some(&(function, _)) = FOOTER.get((mouse.column - areas[5].x) as usize / 12) {
                return self.key(
                    KeyEvent::new(KeyCode::F(function), KeyModifiers::NONE),
                    height,
                    p,
                );
            }
        }
        let grid = areas[1].inner(Margin::new(1, 1));
        if !grid.contains((mouse.column, mouse.row).into()) {
            return Ok(false);
        }
        match mouse.kind {
            MouseEventKind::ScrollDown | MouseEventKind::ScrollUp
                if mouse.modifiers.contains(KeyModifiers::SHIFT) =>
            {
                self.col = if mouse.kind == MouseEventKind::ScrollDown {
                    self.col.saturating_add(3)
                } else {
                    self.col.saturating_sub(3)
                };
                self.last_click = None;
            }
            MouseEventKind::ScrollDown | MouseEventKind::ScrollUp => {
                self.pending_row = None;
                self.last_click = None;
                self.row = if mouse.kind == MouseEventKind::ScrollDown {
                    self.row.saturating_add(3).min(p.rows.saturating_sub(1))
                } else {
                    self.row.saturating_sub(3)
                };
                self.top = if mouse.kind == MouseEventKind::ScrollDown {
                    self.top
                        .saturating_add(3)
                        .min(p.rows.saturating_sub(height as u64))
                } else {
                    self.top.saturating_sub(3)
                };
            }
            MouseEventKind::ScrollLeft => {
                self.col = self.col.saturating_sub(3);
                self.last_click = None;
            }
            MouseEventKind::ScrollRight => {
                self.col = self.col.saturating_add(3);
                self.last_click = None;
            }
            MouseEventKind::Down(MouseButton::Left) | MouseEventKind::Drag(MouseButton::Left) => {
                let Some(relative) = mouse.column.checked_sub(grid.x + 11) else {
                    return Ok(false);
                };
                let slot = relative / (self.width + 1);
                if relative % (self.width + 1) >= self.width
                    || slot as usize >= grid_columns(grid, self.width)
                {
                    return Ok(false);
                }
                let col = self.left + slot as usize;
                if mouse.row == grid.y {
                    if mouse.kind == MouseEventKind::Down(MouseButton::Left) {
                        self.col = col;
                        self.begin_sort();
                    }
                    return Ok(false);
                }
                let row = self.top + (mouse.row - grid.y - 1) as u64;
                if self
                    .rows
                    .get((row - self.top) as usize)
                    .and_then(|r| r.get(col))
                    .is_none()
                {
                    return Ok(false);
                }
                self.row = row;
                self.col = col;
                self.pending_row = None;
                let now = Instant::now();
                let double = mouse.kind == MouseEventKind::Down(MouseButton::Left)
                    && self.last_click.is_some_and(|(r, c, time)| {
                        r == row
                            && c == col
                            && now.duration_since(time) < Duration::from_millis(400)
                    });
                self.last_click = if double || matches!(mouse.kind, MouseEventKind::Drag(_)) {
                    None
                } else {
                    Some((row, col, now))
                };
                if double {
                    self.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), height, p)?;
                }
            }
            _ => {}
        }
        Ok(false)
    }

    fn begin_save(&mut self) {
        if self.save.is_some() || self.sorting.is_some() {
            return;
        }
        let source = &self.sheet.path;
        let stem = source.file_stem().unwrap_or_default().to_string_lossy();
        let extension = source.extension().unwrap_or_default().to_string_lossy();
        let name = if extension.is_empty() {
            format!("{stem}.edited")
        } else {
            format!("{stem}.edited.{extension}")
        };
        self.prompt = Some(Prompt::new(
            Action::Save,
            source.with_file_name(name).to_string_lossy().into_owned(),
        ));
    }

    fn draw(&self, frame: &mut Frame, p: &Progress, columns: usize) {
        let areas = screen_layout(frame.area());
        let grid = Style::default().fg(GRID_FG).bg(GRID_BG);
        let bar = Style::default().fg(BAR_FG).bg(BAR_BG);
        frame.render_widget(Block::new().style(grid), frame.area());
        frame.render_widget(
            Paragraph::new(format!(
                " Quadrille  {}",
                safe(&self.sheet.path.display().to_string())
            ))
            .style(bar),
            areas[0],
        );
        let mut header = vec![Cell::from("Row")];
        header.extend((self.left..self.left + columns).map(|c| {
            let marker = self
                .sheet
                .sort_order()
                .and_then(|o| {
                    o.keys
                        .iter()
                        .position(|k| k.column == c)
                        .map(|i| (i, &o.keys[i]))
                })
                .map(|(i, k)| {
                    format!(
                        " {}{}{}",
                        if k.descending { "↓" } else { "↑" },
                        i + 1,
                        if k.numeric { "n" } else { "" }
                    )
                })
                .unwrap_or_default();
            Cell::from(format!("{}{marker}", column_name(c)))
        }));
        let rows = self.rows.iter().enumerate().map(|(r, record)| {
            let number = self.top + r as u64;
            let mut cells = vec![
                Cell::from((number + 1).to_string()).style(Style::default().fg(ROW_FG).bg(GRID_BG)),
            ];
            for col in self.left..self.left + columns {
                let text = record
                    .get(col)
                    .map(|v| self.sheet.value(number, col, v))
                    .unwrap_or("");
                let style = if number == self.row && col == self.col {
                    Style::default()
                        .fg(SELECTED_FG)
                        .bg(SELECTED_BG)
                        .add_modifier(Modifier::BOLD)
                } else if self.sheet.is_edited(number, col) {
                    Style::default()
                        .fg(EDITED_FG)
                        .bg(GRID_BG)
                        .add_modifier(Modifier::UNDERLINED)
                } else {
                    grid
                };
                cells.push(Cell::from(safe(text)).style(style));
            }
            Row::new(cells)
        });
        let widths = std::iter::once(Constraint::Length(10))
            .chain((0..columns).map(|_| Constraint::Length(self.width)));
        let table = Table::new(rows, widths)
            .style(grid)
            .flex(Flex::Start)
            .header(Row::new(header).style(bar))
            .block(
                Block::bordered()
                    .style(grid)
                    .title(match self.sheet.sheet_name() {
                        Some(name) => format!(
                            " {} · {} · w Sheets · edits are text · formulas read-only ",
                            self.sheet.format(),
                            safe(name)
                        ),
                        None => " CSV · all cells are text ".into(),
                    }),
            );
        frame.render_widget(table, areas[1]);
        let cell = format!("{}{}", column_name(self.col), self.row + 1);
        let mut value = self.current().map(safe).unwrap_or_default();
        if let Some(formula) = self.sheet.formula(self.row, self.col) {
            value = format!("={} · cached: {value}", safe(formula));
        }
        frame.render_widget(
            Paragraph::new(format!("{cell}: {value}"))
                .style(grid)
                .wrap(Wrap { trim: false }),
            areas[2],
        );
        let percent = if p.total_bytes == 0 {
            100.0
        } else {
            p.bytes as f64 / p.total_bytes as f64 * 100.0
        };
        let status = if let Some(error) = &p.error {
            format!("INDEX ERROR: {error}")
        } else if let Some(job) = &self.save {
            if self.sheet.sheet_name().is_some() {
                "Saving workbook/export · navigation available".into()
            } else {
                format!(
                    "Saving {:.1}% · navigation available",
                    job.bytes.load(Ordering::Relaxed) as f64 / p.total_bytes.max(1) as f64 * 100.0
                )
            }
        } else if let Some(job) = &self.sorting {
            let done = job.rows.load(Ordering::Relaxed);
            if done == p.rows {
                "Ordering keys… · Esc cancels".into()
            } else {
                format!(
                    "Reading sort keys {:.1}% · Esc cancels",
                    done as f64 / p.rows.max(1) as f64 * 100.0
                )
            }
        } else {
            format!(
                "{} records{} · {:.1}% indexed · {} changed cells{}{}",
                p.rows,
                if p.done { "" } else { "+" },
                percent,
                self.sheet.edit_count(),
                if self.unsaved { " · unsaved" } else { "" },
                if self.sheet.sort_order().is_some() {
                    " · sorted view"
                } else {
                    ""
                }
            )
        };
        frame.render_widget(Paragraph::new(safe(&status)).style(bar), areas[3]);
        frame.render_widget(Paragraph::new(safe(&self.message)).style(grid), areas[4]);
        let bar = FOOTER
            .iter()
            .map(|(_, label)| format!("{label:<12}"))
            .collect::<String>();
        frame.render_widget(
            Paragraph::new(bar).style(Style::default().fg(BAR_FG).bg(BAR_BG)),
            areas[5],
        );
        if let Some(prompt) = &self.prompt {
            draw_prompt(frame, prompt);
        }
        if let Some(help) = self.help {
            draw_help(frame, help);
        }
    }
}

const FOOTER: [(u8, &str); 5] = [
    (1, "F1 Help"),
    (2, "F2 Edit"),
    (4, "F4 Save"),
    (6, "F6 Sort"),
    (10, "F10 Quit"),
];

#[derive(Clone, Copy, Default)]
struct Help {
    tab: usize,
    scroll: u16,
}

const PLATFORM_HELP: &str = if cfg!(target_os = "macos") {
    "MACOS KEYBOARD\n\
Ctrl means Control (⌃), not Command (⌘).\n\
Fn+← / Fn+→              Home / End (first / last column)\n\
Fn+↑ / Fn+↓              PageUp / PageDown (one screen)\n\
Ctrl+Fn+← / Ctrl+Fn+→    First / last indexed record\n\
F1–F12 may need Fn, depending on keyboard settings.\n\
Terminal shortcuts can intercept keys; Ctrl+G also jumps to a record."
} else {
    "KEYBOARD\n\
Ctrl means Control. Terminal shortcuts can intercept keys."
};

const HELP_TITLES: [&str; 4] = ["Navigation", "Editing", "Sorting", "Workbooks"];

fn help_lines(tab: usize) -> Vec<&'static str> {
    match tab {
        0 => PLATFORM_HELP
            .lines()
            .chain([""])
            .chain(NAVIGATION_HELP.iter().copied())
            .collect(),
        1 => EDITING_HELP.to_vec(),
        2 => SORTING_HELP.to_vec(),
        _ => WORKBOOK_HELP.to_vec(),
    }
}

const NAVIGATION_HELP: &[&str] = &[
    "NAVIGATION",
    "Arrows / Tab / Shift+Tab   Move between cells",
    "PageUp / PageDown         Move one screen",
    "Home / End               First / last column",
    "Ctrl+Home / Ctrl+End     First / last indexed record",
    "Ctrl+G                   Go to a record number",
    "+ / -                    Widen / narrow columns",
    "w                        Choose workbook sheet (save edits first)",
    "",
    "MOUSE",
    "Click / drag             Select a cell",
    "Double-click             Edit a cell",
    "Wheel                    Scroll rows",
    "Shift+wheel / sideways   Scroll columns",
    "Click column heading     Open sort dialog",
    "Click function-key bar   Run that command",
    "",
    "QUIT",
    "q / Ctrl+Q / Ctrl+C / F10  Quit (confirm unsaved changes)",
    "? / h / F1               Help; Esc closes help",
];

const EDITING_HELP: &[&str] = &[
    "EDIT AND SAVE",
    "Enter / F2               Edit selected cell",
    "Ctrl+Z                   Undo last cell edit",
    "Ctrl+S / F4              Save As (new file only)",
    "Editor: Ctrl+A select all; Ctrl+J newline; Enter apply; Esc cancel",
    "",
    "VALUES",
    "Rows include the header. CSV values and new workbook edits are text.",
    "Underlined yellow cells contain edits; amber marks the selected cell.",
];

const SORTING_HELP: &[&str] = &[
    "SORT",
    "s / F6                   Sort by one or more columns",
    "B,-D:n                   B text ascending, then D numeric descending",
    "F2 in sort dialog        Toggle first-record header",
    "clear in sort dialog     Restore source order; keep cell edits",
    "Esc while sorting        Cancel; keep previous view",
    "Sort uses current values; editing a key does not automatically re-sort.",
    "Save As writes visible order; sorted saves normalize record endings to LF.",
    "Empty/missing keys sort last. Numeric keys require exact decimals.",
];

const WORKBOOK_HELP: &[&str] = &[
    "WORKBOOKS",
    "w                        Choose an XLSX / ODS sheet",
    "Save workbook edits before switching sheets.",
    "Workbooks: formulas are read-only cached results; qd does not recalculate.",
    "Native saves preserve the workbook; clear sorting first. Export views as .csv.",
];

fn screen_layout(area: Rect) -> [Rect; 6] {
    Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(3),
        Constraint::Length(2),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .areas(area)
}

fn grid_columns(inner: Rect, width: u16) -> usize {
    (inner.width.saturating_sub(10) / (width + 1)).max(1) as usize
}

fn help_area(area: Rect) -> Rect {
    let width = area.width.saturating_sub(2).min(96);
    let height = area.height.saturating_sub(2).min(36);
    Rect::new(
        area.x + (area.width - width) / 2,
        area.y + (area.height - height) / 2,
        width,
        height,
    )
}

fn help_layout(area: Rect) -> [Rect; 3] {
    Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(0),
    ])
    .areas(help_area(area).inner(Margin::new(2, 2)))
}

fn draw_help(frame: &mut Frame, help: Help) {
    let popup = help_area(frame.area());
    let areas = help_layout(frame.area());
    let background = Style::default().fg(PANEL_FG).bg(PANEL_BG);
    let lines: Vec<_> = help_lines(help.tab)
        .into_iter()
        .map(|text| {
            let style = if matches!(
                text,
                "MACOS KEYBOARD"
                    | "KEYBOARD"
                    | "NAVIGATION"
                    | "MOUSE"
                    | "EDIT AND SAVE"
                    | "VALUES"
                    | "SORT"
                    | "QUIT"
                    | "WORKBOOKS"
            ) {
                Style::default()
                    .fg(PANEL_ACCENT)
                    .bg(PANEL_BG)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            };
            Line::styled(text, style)
        })
        .collect();
    frame.render_widget(Clear, popup);
    frame.render_widget(
        Block::bordered()
            .style(background)
            .border_style(Style::default().fg(PANEL_BORDER).bg(PANEL_BG))
            .title(" Commands · ←/→ tabs · ↑/↓ / wheel scroll · Esc close "),
        popup,
    );
    frame.render_widget(
        Tabs::new(HELP_TITLES)
            .select(help.tab)
            .divider(" │ ")
            .style(background)
            .highlight_style(
                Style::default()
                    .fg(PANEL_ACCENT)
                    .bg(PANEL_BG)
                    .add_modifier(Modifier::BOLD | Modifier::UNDERLINED),
            ),
        areas[0],
    );
    frame.render_widget(
        Paragraph::new(lines)
            .style(background)
            .wrap(Wrap { trim: false })
            .scroll((help.scroll, 0)),
        areas[2],
    );
}

fn draw_prompt(frame: &mut Frame, prompt: &Prompt) {
    let area = frame.area();
    let width = area.width.saturating_sub(4).min(100);
    let height = area.height.min(6 + prompt.choices.len().min(20) as u16);
    let popup = Rect::new(
        area.x + (area.width - width) / 2,
        area.y + (area.height - height) / 2,
        width,
        height,
    );
    let title = match prompt.action {
        Action::Edit => " Edit cell · Enter apply · Esc cancel · Ctrl+J newline ",
        Action::Save => " Save as NEW file · Enter save · Esc cancel ",
        Action::Goto => " Go to row (1-based, including header) ",
        Action::Quit => " Discard unsaved changes? y / n ",
        Action::Sort => " Sort columns · Enter apply · Esc cancel ",
        Action::Sheet => " Sheet · ↑/↓ or name/number · Enter open · Esc cancel ",
    };
    frame.render_widget(Clear, popup);
    let panel = Style::default().fg(PANEL_FG).bg(PANEL_BG);
    let block = Block::bordered()
        .style(panel)
        .border_style(Style::default().fg(PANEL_BORDER).bg(PANEL_BG))
        .title(Line::styled(
            title,
            Style::default()
                .fg(PANEL_FG)
                .bg(PANEL_BG)
                .add_modifier(Modifier::BOLD),
        ));
    let inner = block.inner(popup);
    frame.render_widget(block, popup);
    if inner.width == 0 || inner.height == 0 {
        return;
    }
    let before = safe(&prompt.text[..prompt.cursor]);
    let cursor_width = UnicodeWidthStr::width(before.as_str());
    let scroll = cursor_width.saturating_sub(inner.width.saturating_sub(1) as usize);
    let display = safe(&prompt.text);
    let input = Style::default().fg(INPUT_FG).bg(INPUT_BG);
    let text = if prompt.select_all {
        Style::default()
            .fg(SELECTED_FG)
            .bg(SELECTED_BG)
            .add_modifier(Modifier::BOLD)
    } else {
        input
    };
    if prompt.action != Action::Quit {
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(display, text)))
                .style(input)
                .scroll((0, scroll.min(u16::MAX as usize) as u16)),
            Rect::new(inner.x, inner.y, inner.width, 1),
        );
    }
    if prompt.action != Action::Quit {
        frame.set_cursor_position((inner.x + (cursor_width - scroll) as u16, inner.y));
    }
    if inner.height > 2 && prompt.action == Action::Sheet {
        let selected = prompt
            .choices
            .iter()
            .position(|s| s == &prompt.text)
            .unwrap_or(0);
        let names = prompt
            .choices
            .iter()
            .enumerate()
            .map(|(i, name)| format!("{}  {}", i + 1, safe(name)))
            .collect::<Vec<_>>()
            .join("\n");
        frame.render_widget(
            Paragraph::new(names).style(panel).scroll((
                selected
                    .saturating_sub(inner.height.saturating_sub(3) as usize)
                    .min(u16::MAX as usize) as u16,
                0,
            )),
            Rect::new(inner.x, inner.y + 2, inner.width, inner.height - 2),
        );
    }
    if inner.height > 2 && prompt.action == Action::Sort {
        frame.render_widget(
            Paragraph::new(format!(
                "B,-D:n = B text ↑, D numeric ↓ · F2 Header: {}",
                if prompt.header { "ON" } else { "OFF" }
            ))
            .style(panel),
            Rect::new(inner.x, inner.y + 1, inner.width, 1),
        );
        frame.render_widget(
            Paragraph::new("Enter clear to restore source order · saves follow the visible order")
                .style(panel),
            Rect::new(inner.x, inner.y + 2, inner.width, 1),
        );
    }
    if inner.height > 2 && prompt.action == Action::Edit {
        frame.render_widget(
            Paragraph::new("Typing replaces selection · ←/→ move · Ctrl+A select all").style(panel),
            Rect::new(inner.x, inner.y + 2, inner.width, 1),
        );
    }
}

fn safe(text: &str) -> String {
    text.chars()
        .map(|c| match c {
            '\n' => '↵',
            '\r' => '␍',
            '\t' => '⇥',
            c if c.is_control() => '�',
            c => c,
        })
        .collect()
}

fn column_name(mut col: usize) -> String {
    let mut name = Vec::new();
    loop {
        name.push(b'A' + (col % 26) as u8);
        if col < 26 {
            break;
        }
        col = col / 26 - 1;
    }
    name.reverse();
    String::from_utf8(name).unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{Terminal, backend::TestBackend};

    fn contrast(foreground: Color, background: Color) -> f64 {
        fn luminance(color: Color) -> f64 {
            let Color::Rgb(red, green, blue) = color else {
                panic!("Accessibility palette colors must be explicit RGB")
            };
            [red, green, blue]
                .into_iter()
                .zip([0.2126, 0.7152, 0.0722])
                .map(|(channel, weight)| {
                    let channel = f64::from(channel) / 255.0;
                    weight
                        * if channel <= 0.04045 {
                            channel / 12.92
                        } else {
                            ((channel + 0.055) / 1.055).powf(2.4)
                        }
                })
                .sum()
        }
        let (light, dark) = {
            let a = luminance(foreground);
            let b = luminance(background);
            (a.max(b), a.min(b))
        };
        (light + 0.05) / (dark + 0.05)
    }

    #[test]
    fn palette_has_enhanced_text_contrast_and_visible_boundaries() {
        for (foreground, background) in [
            (GRID_FG, GRID_BG),
            (ROW_FG, GRID_BG),
            (EDITED_FG, GRID_BG),
            (SELECTED_FG, SELECTED_BG),
            (BAR_FG, BAR_BG),
            (PANEL_FG, PANEL_BG),
            (PANEL_ACCENT, PANEL_BG),
            (INPUT_FG, INPUT_BG),
        ] {
            assert!(contrast(foreground, background) >= 7.0);
        }
        assert!(contrast(PANEL_BORDER, PANEL_BG) >= 3.0);
        assert!(contrast(SELECTED_BG, GRID_BG) >= 3.0);
    }

    #[test]
    fn unicode_edit_and_small_terminal_render() {
        let mut p = Prompt::new(Action::Edit, "old".into());
        p.insert("Tallinn 🦀");
        p.key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE));
        assert_eq!(p.text, "Tallinn ");
        p.insert("õ");
        p.key(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE));
        p.key(KeyEvent::new(KeyCode::Delete, KeyModifiers::NONE));
        assert_eq!(p.text, "Tallinn ");
        assert_eq!(column_name(0), "A");
        assert_eq!(column_name(26), "AA");
        for (w, h) in [(1, 1), (20, 5), (100, 30)] {
            let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
            terminal.draw(|f| draw_prompt(f, &p)).unwrap();
        }
    }
    #[test]
    fn help_is_modal_and_mouse_uses_the_rendered_grid() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("data.csv");
        std::fs::write(&path, "a,b,c\n1,2,3\n4,5,6\n").unwrap();
        let mut sheet = Sheet::open(&path, b',').unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while !sheet.progress().done {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(1));
        }
        let records = sheet.window(0, 3).unwrap();
        let p = sheet.progress();
        let mut app = App::new(sheet);
        app.rows = records;
        for code in [KeyCode::Char('?'), KeyCode::Char('h'), KeyCode::F(1)] {
            app.key(KeyEvent::new(code, KeyModifiers::NONE), 19, &p)
                .unwrap();
            assert!(app.help.is_some());
            app.key(
                KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL),
                19,
                &p,
            )
            .unwrap();
            assert!(app.prompt.is_none());
            app.key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), 19, &p)
                .unwrap();
            assert!(app.help.is_none());
        }
        app.key(
            KeyEvent::new(KeyCode::Char('?'), KeyModifiers::NONE),
            19,
            &p,
        )
        .unwrap();
        app.key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE), 19, &p)
            .unwrap();
        assert_eq!(app.help.unwrap().tab, 1);
        app.key(KeyEvent::new(KeyCode::BackTab, KeyModifiers::NONE), 19, &p)
            .unwrap();
        assert_eq!(app.help.unwrap().tab, 0);
        app.key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), 19, &p)
            .unwrap();
        let screen = Rect::new(0, 0, 110, 28);
        let click = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 37,
            row: 4,
            modifiers: KeyModifiers::NONE,
        };
        app.mouse(click, screen, 19, &p).unwrap();
        assert_eq!((app.row, app.col), (1, 1));
        app.mouse(click, screen, 19, &p).unwrap();
        assert!(matches!(
            app.prompt.as_ref().map(|p| p.action),
            Some(Action::Edit)
        ));
        app.prompt = None;
        app.mouse(
            MouseEvent {
                kind: MouseEventKind::Drag(MouseButton::Left),
                ..click
            },
            screen,
            19,
            &p,
        )
        .unwrap();
        app.mouse(click, screen, 19, &p).unwrap();
        assert!(
            app.prompt.is_none(),
            "A drag followed by a click is not a double-click"
        );
        let header = MouseEvent { row: 2, ..click };
        app.mouse(header, screen, 19, &p).unwrap();
        assert!(matches!(
            app.prompt.as_ref().map(|p| p.action),
            Some(Action::Sort)
        ));
        app.prompt = None;
        app.mouse(
            MouseEvent {
                kind: MouseEventKind::ScrollDown,
                ..click
            },
            screen,
            19,
            &p,
        )
        .unwrap();
        assert_eq!(app.row, 2);
        for (w, h) in [(1, 1), (20, 5), (110, 28)] {
            let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
            terminal.draw(|f| app.draw(f, &p, 3)).unwrap();
            for tab in 0..HELP_TITLES.len() {
                terminal
                    .draw(|f| draw_help(f, Help { tab, scroll: 0 }))
                    .unwrap();
            }
        }
    }
}
