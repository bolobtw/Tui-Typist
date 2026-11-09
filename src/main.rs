use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind, KeyModifiers},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use mlua::Lua;
use rand::seq::SliceRandom;
use ratatui::{
    Terminal,
    backend::{Backend, CrosstermBackend},
    layout::{Alignment, Constraint, Direction, Flex, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, Borders, Cell, Clear, Padding, Paragraph, Row, Table, Wrap},
};
use serde::{Deserialize, Serialize};
use std::{
    env,
    fs::{self, OpenOptions},
    io::{self, BufRead, BufReader, Write},
    path::PathBuf,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

const DEFAULT_WORDS: &str = include_str!("../wordlists/words.txt");
const HISTORY_FILE: &str = "history.jsonl";
const TIME_PRESETS: [u64; 3] = [30, 60, 90];

#[derive(PartialEq)]
enum AppState {
    Typing,
    Finished,
}

#[derive(Clone, Serialize, Deserialize)]
struct RunStats {
    wpm: f64,
    raw_wpm: f64,
    accuracy: f64,
    correct_chars: usize,
    total_chars: usize,
    duration_secs: f64,
    wordlist_name: String,
    timestamp: u64,
}

#[derive(Clone)]
struct Theme {
    header_fg: Color,
    correct_fg: Color,
    incorrect_fg: Color,
    incorrect_bg: Color,
    untyped_fg: Color,
    active_fg: Color,
    active_bg: Color,
    border_fg: Color,
}

impl Default for Theme {
    fn default() -> Self {
        Self {
            header_fg: Color::White,
            correct_fg: Color::Green,
            incorrect_fg: Color::Red,
            incorrect_bg: Color::Rgb(80, 0, 0),
            untyped_fg: Color::DarkGray,
            active_fg: Color::Black,
            active_bg: Color::White,
            border_fg: Color::White,
        }
    }
}

impl Theme {
    fn from_lua_script(script: &str) -> Self {
        let mut theme = Theme::default();
        let lua = Lua::new();
        if let Ok(table) = lua.load(script).eval::<mlua::Table>() {
            let get_color = |key: &str, default: Color| -> Color {
                table
                    .get::<String>(key)
                    .ok()
                    .and_then(|s| parse_hex_color(&s))
                    .unwrap_or(default)
            };

            theme.header_fg = get_color("header", theme.header_fg);
            theme.correct_fg = get_color("correct", theme.correct_fg);
            theme.incorrect_fg = get_color("incorrect_fg", theme.incorrect_fg);
            theme.incorrect_bg = get_color("incorrect_bg", theme.incorrect_bg);
            theme.untyped_fg = get_color("untyped", theme.untyped_fg);
            theme.active_fg = get_color("active_fg", theme.active_fg);
            theme.active_bg = get_color("active_bg", theme.active_bg);
            theme.border_fg = get_color("border", theme.border_fg);
        }
        theme
    }
}

fn parse_hex_color(hex: &str) -> Option<Color> {
    let hex = hex.trim_start_matches('#');
    if hex.len() == 6 {
        let r = u8::from_str_radix(&hex[0..2], 16).ok()?;
        let g = u8::from_str_radix(&hex[2..4], 16).ok()?;
        let b = u8::from_str_radix(&hex[4..6], 16).ok()?;
        Some(Color::Rgb(r, g, b))
    } else {
        None
    }
}

struct App {
    state: AppState,
    show_help: bool,
    target_words: String,
    user_input: String,
    start_time: Option<Instant>,
    preset_idx: usize,
    time_limit: Duration,
    total_keystrokes: usize,
    correct_keystrokes: usize,
    last_run: Option<RunStats>,
    history: Vec<RunStats>,
    available_wordlists: Vec<(String, String)>,
    current_wordlist_idx: usize,
    available_themes: Vec<(String, Theme)>,
    current_theme_idx: usize,
}

impl App {
    fn new(cli_wordlist: Option<String>, cli_theme: Option<String>) -> App {
        let mut available_wordlists = Vec::new();
        if let Ok(entries) = fs::read_dir("wordlists") {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_file() && path.extension().and_then(|s| s.to_str()) == Some("txt") {
                    if let Ok(content) = fs::read_to_string(&path) {
                        if !content.trim().is_empty() {
                            let name = path
                                .file_stem()
                                .and_then(|s| s.to_str())
                                .unwrap_or("words")
                                .to_string();
                            available_wordlists.push((name, content));
                        }
                    }
                }
            }
        }

        available_wordlists.sort_by(|a, b| a.0.cmp(&b.0));
        if available_wordlists.is_empty() {
            available_wordlists.push(("default".to_string(), DEFAULT_WORDS.to_string()));
        }

        let mut current_wordlist_idx = available_wordlists
            .iter()
            .position(|(name, _)| name == "words" || name == "default")
            .unwrap_or(0);

        if let Some(path_str) = cli_wordlist {
            if let Ok(content) = fs::read_to_string(&path_str) {
                let name = PathBuf::from(&path_str)
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("cli")
                    .to_string();
                if let Some(pos) = available_wordlists.iter().position(|(n, _)| *n == name) {
                    current_wordlist_idx = pos;
                } else {
                    available_wordlists.push((name, content));
                    current_wordlist_idx = available_wordlists.len() - 1;
                }
            }
        }

        let mut available_themes = vec![("default".to_string(), Theme::default())];
        let mut custom_themes = Vec::new();

        if let Ok(entries) = fs::read_dir("themes") {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_file() && path.extension().and_then(|s| s.to_str()) == Some("lua") {
                    if let Ok(script) = fs::read_to_string(&path) {
                        let name = path
                            .file_stem()
                            .and_then(|s| s.to_str())
                            .unwrap_or("theme")
                            .to_string();
                        custom_themes.push((name, Theme::from_lua_script(&script)));
                    }
                }
            }
        }
        custom_themes.sort_by(|a, b| a.0.cmp(&b.0));
        available_themes.extend(custom_themes);

        let mut current_theme_idx = available_themes
            .iter()
            .position(|(name, _)| name == "gruvbox")
            .unwrap_or(if available_themes.len() > 1 { 1 } else { 0 });

        if let Some(path_str) = cli_theme {
            if let Ok(script) = fs::read_to_string(&path_str) {
                let name = PathBuf::from(&path_str)
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("cli")
                    .to_string();
                if let Some(pos) = available_themes.iter().position(|(n, _)| *n == name) {
                    current_theme_idx = pos;
                } else {
                    available_themes.push((name, Theme::from_lua_script(&script)));
                    current_theme_idx = available_themes.len() - 1;
                }
            }
        }

        let preset_idx = 0;
        let mut app = App {
            state: AppState::Typing,
            show_help: false,
            target_words: String::new(),
            user_input: String::new(),
            start_time: None,
            preset_idx,
            time_limit: Duration::from_secs(TIME_PRESETS[preset_idx]),
            total_keystrokes: 0,
            correct_keystrokes: 0,
            last_run: None,
            history: load_history(),
            available_wordlists,
            current_wordlist_idx,
            available_themes,
            current_theme_idx,
        };
        app.reset_run();
        app
    }

    fn theme(&self) -> &Theme {
        &self.available_themes[self.current_theme_idx].1
    }

    fn theme_name(&self) -> &str {
        &self.available_themes[self.current_theme_idx].0
    }

    fn reset_run(&mut self) {
        let (_, words_str) = &self.available_wordlists[self.current_wordlist_idx];
        let base_words: Vec<&str> = words_str.split_whitespace().collect();
        let mut rng = rand::rng();

        let target_word_count = (TIME_PRESETS[self.preset_idx] as usize / 30) * 50;

        let mut selected_words = Vec::with_capacity(target_word_count);
        if !base_words.is_empty() {
            while selected_words.len() < target_word_count {
                let mut pool = base_words.clone();
                pool.shuffle(&mut rng);
                let needed = target_word_count - selected_words.len();
                selected_words.extend_from_slice(&pool[..needed.min(pool.len())]);
            }
            self.target_words = selected_words.join(" ");
        } else {
            self.target_words = "the quick brown fox jumps over the lazy dog".to_string();
        }

        self.user_input.clear();
        self.start_time = None;
        self.total_keystrokes = 0;
        self.correct_keystrokes = 0;
        self.state = AppState::Typing;
    }

    fn cycle_wordlist(&mut self) {
        self.current_wordlist_idx = (self.current_wordlist_idx + 1) % self.available_wordlists.len();
        self.reset_run();
    }

    fn cycle_time_preset(&mut self) {
        self.preset_idx = (self.preset_idx + 1) % TIME_PRESETS.len();
        self.time_limit = Duration::from_secs(TIME_PRESETS[self.preset_idx]);
        self.reset_run();
    }

    fn cycle_theme(&mut self) {
        self.current_theme_idx = (self.current_theme_idx + 1) % self.available_themes.len();
    }

    fn current_metrics(&self) -> (f64, f64, f64, usize) {
        let elapsed_secs = self
            .start_time
            .map(|s| s.elapsed().as_secs_f64())
            .unwrap_or(0.0);

        let correct_on_screen = self
            .target_words
            .chars()
            .zip(self.user_input.chars())
            .filter(|(t, u)| t == u)
            .count();

        let (wpm, raw_wpm) = if elapsed_secs > 0.5 {
            let mins = elapsed_secs / 60.0;
            (
                (correct_on_screen as f64 / 5.0) / mins,
                (self.total_keystrokes as f64 / 5.0) / mins,
            )
        } else {
            (0.0, 0.0)
        };

        let accuracy = if self.total_keystrokes > 0 {
            (self.correct_keystrokes as f64 / self.total_keystrokes as f64) * 100.0
        } else {
            100.0
        };

        (wpm, raw_wpm, accuracy, correct_on_screen)
    }

    fn finish_run(&mut self) {
        if self.state == AppState::Finished {
            return;
        }
        let elapsed_secs = self
            .start_time
            .map(|s| s.elapsed().as_secs_f64().min(self.time_limit.as_secs_f64()))
            .unwrap_or(0.0);

        let (wpm, raw_wpm, accuracy, correct_chars) = self.current_metrics();
        let (wordlist_name, _) = &self.available_wordlists[self.current_wordlist_idx];

        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);

        let stats = RunStats {
            wpm,
            raw_wpm,
            accuracy,
            correct_chars,
            total_chars: self.total_keystrokes,
            duration_secs: elapsed_secs,
            wordlist_name: wordlist_name.clone(),
            timestamp,
        };

        save_run_to_history(&stats);
        self.history.push(stats.clone());
        self.last_run = Some(stats);
        self.state = AppState::Finished;
    }
}

fn load_history() -> Vec<RunStats> {
    let Ok(file) = fs::File::open(HISTORY_FILE) else {
        return Vec::new();
    };
    BufReader::new(file)
        .lines()
        .map_while(Result::ok)
        .filter_map(|line| serde_json::from_str::<RunStats>(&line).ok())
        .collect()
}

fn save_run_to_history(stats: &RunStats) {
    if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(HISTORY_FILE) {
        if let Ok(json) = serde_json::to_string(stats) {
            let _ = writeln!(file, "{}", json);
        }
    }
}

fn main() -> Result<(), io::Error> {
    let args: Vec<String> = env::args().collect();
    let cli_wordlist = args.get(1).cloned();
    let cli_theme = args.get(2).cloned();

    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let mut app = App::new(cli_wordlist, cli_theme);
    let res = run_app(&mut terminal, &mut app);

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    if let Err(err) = res {
        println!("{:?}", err);
    }

    Ok(())
}

fn run_app<B: Backend>(terminal: &mut Terminal<B>, app: &mut App) -> io::Result<()>
where
    std::io::Error: From<<B as Backend>::Error>,
{
    loop {
        terminal.draw(|f| ui(f, app))?;

        if event::poll(Duration::from_millis(50))? {
            if let Event::Key(key) = event::read()? {
                if key.kind != KeyEventKind::Press {
                    continue;
                }

                if key.modifiers == KeyModifiers::CONTROL && key.code == KeyCode::Char('k') {
                    app.show_help = !app.show_help;
                    continue;
                }

                if app.show_help {
                    if matches!(key.code, KeyCode::Esc | KeyCode::Enter) {
                        app.show_help = false;
                    }
                    continue;
                }

                match (key.modifiers, key.code) {
                    (_, KeyCode::Esc) => return Ok(()),
                    (_, KeyCode::Tab) => {
                        app.reset_run();
                        continue;
                    }
                    (KeyModifiers::CONTROL, KeyCode::Char('w')) => {
                        app.cycle_wordlist();
                        continue;
                    }
                    (KeyModifiers::CONTROL, KeyCode::Char('d')) => {
                        app.cycle_time_preset();
                        continue;
                    }
                    (KeyModifiers::CONTROL, KeyCode::Char('t')) => {
                        app.cycle_theme();
                        continue;
                    }
                    _ => {}
                }

                match app.state {
                    AppState::Typing => match key.code {
                        KeyCode::Backspace => {
                            app.user_input.pop();
                        }
                        KeyCode::Char(c) => {
                            let target_chars: Vec<char> = app.target_words.chars().collect();
                            let input_len = app.user_input.chars().count();

                            if input_len < target_chars.len() {
                                if app.start_time.is_none() {
                                    app.start_time = Some(Instant::now());
                                }
                                app.total_keystrokes += 1;
                                if target_chars[input_len] == c {
                                    app.correct_keystrokes += 1;
                                }
                                app.user_input.push(c);

                                if app.user_input.chars().count() == target_chars.len() {
                                    app.finish_run();
                                }
                            }
                        }
                        _ => {}
                    },
                    AppState::Finished => {
                        if key.code == KeyCode::Enter {
                            app.reset_run();
                        }
                    }
                }
            }
        }

        if app.state == AppState::Typing && !app.show_help {
            if let Some(start) = app.start_time {
                if start.elapsed() >= app.time_limit {
                    app.finish_run();
                }
            }
        }
    }
}

fn ui(f: &mut ratatui::Frame, app: &App) {
    match app.state {
        AppState::Typing => render_typing_ui(f, app),
        AppState::Finished => render_finish_ui(f, app),
    }

    if app.show_help {
        render_help_modal(f, app);
    }
}

fn render_typing_ui(f: &mut ratatui::Frame, app: &App) {
    let theme = app.theme();
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .margin(2)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(1),
            Constraint::Length(1),
        ])
        .split(f.area());

    let time_left = if let Some(start) = app.start_time {
        let elapsed = start.elapsed().as_secs();
        app.time_limit.as_secs().saturating_sub(elapsed)
    } else {
        app.time_limit.as_secs()
    };

    let (wpm, _, _, _) = app.current_metrics();
    let (wordlist_name, _) = &app.available_wordlists[app.current_wordlist_idx];

    let header_text = format!(
        " Time: {}s | WPM: {:.0} | List: {} | Theme: {} ",
        time_left,
        wpm,
        wordlist_name,
        app.theme_name()
    );

    let header = Paragraph::new(header_text)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(theme.border_fg)),
        )
        .style(Style::default().fg(theme.header_fg));
    f.render_widget(header, chunks[0]);

    let target_chars: Vec<char> = app.target_words.chars().collect();
    let input_chars: Vec<char> = app.user_input.chars().collect();

    let style_correct = Style::default().fg(theme.correct_fg);
    let style_incorrect = Style::default()
        .fg(theme.incorrect_fg)
        .bg(theme.incorrect_bg);
    let style_untyped = Style::default().fg(theme.untyped_fg);
    let style_active = Style::default()
        .fg(theme.active_fg)
        .bg(theme.active_bg)
        .add_modifier(Modifier::BOLD);

    let mut styled_spans = Vec::with_capacity(target_chars.len());

    for (index, &target_char) in target_chars.iter().enumerate() {
        let span = if index < input_chars.len() {
            if input_chars[index] == target_char {
                Span::styled(target_char.to_string(), style_correct)
            } else {
                Span::styled(target_char.to_string(), style_incorrect)
            }
        } else if index == input_chars.len() {
            Span::styled(target_char.to_string(), style_active)
        } else {
            Span::styled(target_char.to_string(), style_untyped)
        };
        styled_spans.push(span);
    }

    let typing_area = Paragraph::new(Text::from(Line::from(styled_spans)))
        .block(
            Block::default()
                .title(" Tui-Typist ")
                .borders(Borders::ALL)
                .border_style(Style::default().fg(theme.border_fg))
                .padding(Padding::new(1, 1, 1, 1)),
        )
        .wrap(Wrap { trim: false });
    f.render_widget(typing_area, chunks[1]);

    let footer_line = Line::from(vec![
        Span::styled(
            " Ctrl+K ",
            Style::default()
                .fg(theme.active_fg)
                .bg(theme.border_fg)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(" Help ", Style::default().fg(theme.untyped_fg)),
    ]);
    f.render_widget(
        Paragraph::new(footer_line).alignment(Alignment::Right),
        chunks[2],
    );
}

fn render_finish_ui(f: &mut ratatui::Frame, app: &App) {
    let theme = app.theme();
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .margin(2)
        .constraints([Constraint::Length(9), Constraint::Min(5)])
        .split(f.area());

    if let Some(stats) = &app.last_run {
        let summary_lines = vec![
            Line::from(vec![
                Span::styled(" Net WPM:      ", Style::default()),
                Span::styled(
                    format!("{:.1}", stats.wpm),
                    Style::default().add_modifier(Modifier::BOLD),
                ),
            ]),
            Line::from(vec![
                Span::styled(" Raw WPM:      ", Style::default()),
                Span::styled(
                    format!("{:.1}", stats.raw_wpm),
                    Style::default().add_modifier(Modifier::BOLD),
                ),
            ]),
            Line::from(vec![
                Span::styled(" Accuracy:     ", Style::default()),
                Span::styled(
                    format!(
                        "{:.1}% ({}/{})",
                        stats.accuracy, stats.correct_chars, stats.total_chars
                    ),
                    Style::default().add_modifier(Modifier::BOLD),
                ),
            ]),
            Line::from(vec![
                Span::styled(" Time / List:  ", Style::default()),
                Span::styled(
                    format!("{:.1}s on [{}]", stats.duration_secs, stats.wordlist_name),
                    Style::default().add_modifier(Modifier::BOLD),
                ),
            ]),
            Line::from(""),
            Line::from(vec![
                Span::raw(" "),
                Span::styled(
                    " Tab ",
                    Style::default()
                        .fg(theme.active_fg)
                        .bg(theme.border_fg)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(" Restart  ", Style::default().fg(theme.untyped_fg)),
                Span::styled(
                    " Ctrl+K ",
                    Style::default()
                        .fg(theme.active_fg)
                        .bg(theme.border_fg)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(" Help  ", Style::default().fg(theme.untyped_fg)),
                Span::styled(
                    " Esc ",
                    Style::default()
                        .fg(theme.active_fg)
                        .bg(theme.border_fg)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(" Quit", Style::default().fg(theme.untyped_fg)),
            ]),
        ];

        let summary_block = Paragraph::new(summary_lines).block(
            Block::default()
                .title(" Run Complete! ")
                .borders(Borders::ALL)
                .border_style(Style::default().fg(theme.border_fg))
                .padding(Padding::new(1, 1, 1, 0)),
        );
        f.render_widget(summary_block, chunks[0]);
    }

    let header_cells = ["Run", "Net WPM", "Raw WPM", "Accuracy", "Time", "Wordlist"]
        .iter()
        .map(|h| {
            Cell::from(*h).style(
                Style::default()
                    .fg(theme.header_fg)
                    .add_modifier(Modifier::BOLD),
            )
        });
    let header = Row::new(header_cells).height(1).bottom_margin(1);

    let total_runs = app.history.len();

    let best_run_idx = app
        .history
        .iter()
        .enumerate()
        .max_by(|(_, a), (_, b)| a.wpm.total_cmp(&b.wpm))
        .map(|(idx, _)| idx);

    let rows = app.history.iter().enumerate().rev().take(10).map(|(idx, run)| {
        let is_best = best_run_idx == Some(idx) && run.wpm > 0.0;
        let row_style = if is_best {
            Style::default()
                .fg(theme.correct_fg)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default()
        };

        Row::new(vec![
            Cell::from(format!("#{}", idx + 1)),
            Cell::from(format!("{:.1}", run.wpm)),
            Cell::from(format!("{:.1}", run.raw_wpm)),
            Cell::from(format!("{:.1}%", run.accuracy)),
            Cell::from(format!("{:.1}s", run.duration_secs)),
            Cell::from(run.wordlist_name.clone()),
        ])
        .style(row_style)
    });

    let table = Table::new(
        rows,
        [
            Constraint::Length(6),
            Constraint::Length(10),
            Constraint::Length(10),
            Constraint::Length(10),
            Constraint::Length(8),
            Constraint::Min(10),
        ],
    )
    .header(header)
    .block(
        Block::default()
            .title(format!(" Past Runs (Total: {}) ", total_runs))
            .borders(Borders::ALL)
            .border_style(Style::default().fg(theme.border_fg))
            .padding(Padding::new(1, 1, 1, 1)),
    );

    f.render_widget(table, chunks[1]);
}

fn render_help_modal(f: &mut ratatui::Frame, app: &App) {
    let theme = app.theme();
    let area = centered_rect(46, 13, f.area());

    let pill_style = Style::default()
        .fg(theme.active_fg)
        .bg(theme.border_fg)
        .add_modifier(Modifier::BOLD);
    let desc_style = Style::default().fg(theme.active_bg);

    let help_lines = vec![
        Line::from(vec![
            Span::styled("  Tab       ", pill_style),
            Span::styled("  Restart current run", desc_style),
        ]),
        Line::from(""),
        Line::from(vec![
            Span::styled("  Ctrl+D    ", pill_style),
            Span::styled("  Cycle timer (30s / 60s / 90s)", desc_style),
        ]),
        Line::from(""),
        Line::from(vec![
            Span::styled("  Ctrl+W    ", pill_style),
            Span::styled("  Cycle wordlist", desc_style),
        ]),
        Line::from(""),
        Line::from(vec![
            Span::styled("  Ctrl+T    ", pill_style),
            Span::styled("  Cycle theme", desc_style),
        ]),
        Line::from(""),
        Line::from(vec![
            Span::styled("  Ctrl+K    ", pill_style),
            Span::styled("  Close this help menu", desc_style),
        ]),
        Line::from(""),
        Line::from(vec![
            Span::styled("  Esc       ", pill_style),
            Span::styled("  Quit Tui-Typist", desc_style),
        ]),
    ];

    let modal = Paragraph::new(help_lines).block(
        Block::default()
            .title(" Keybindings ")
            .borders(Borders::ALL)
            .border_style(Style::default().fg(theme.header_fg))
            .padding(Padding::new(1, 1, 1, 1)),
    );

    f.render_widget(Clear, area);
    f.render_widget(modal, area);
}

fn centered_rect(width: u16, height: u16, r: Rect) -> Rect {
    let vertical = Layout::vertical([Constraint::Length(height)])
        .flex(Flex::Center)
        .split(r);
    Layout::horizontal([Constraint::Length(width)])
        .flex(Flex::Center)
        .split(vertical[0])[0]
}