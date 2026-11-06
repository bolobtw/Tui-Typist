use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind, KeyModifiers},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use rand::seq::SliceRandom;
use ratatui::{
    Terminal,
    backend::{Backend, CrosstermBackend},
    layout::{Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, Borders, Cell, Padding, Paragraph, Row, Table, Wrap},
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

struct App {
    state: AppState,
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
}

impl App {
    fn new(cli_wordlist: Option<String>) -> App {
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

        let preset_idx = 0;
        let mut app = App {
            state: AppState::Typing,
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
        };
        app.reset_run();
        app
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

    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let mut app = App::new(cli_wordlist);
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

        if app.state == AppState::Typing {
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
}

fn render_typing_ui(f: &mut ratatui::Frame, app: &App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .margin(2)
        .constraints([Constraint::Length(3), Constraint::Min(1)])
        .split(f.area());

    let time_left = if let Some(start) = app.start_time {
        let elapsed = start.elapsed().as_secs();
        app.time_limit.as_secs().saturating_sub(elapsed)
    } else {
        app.time_limit.as_secs()
    };

    let (wpm, _, _, _) = app.current_metrics();
    let (wordlist_name, _) = &app.available_wordlists[app.current_wordlist_idx];

    let preset_display: Vec<String> = TIME_PRESETS
        .iter()
        .enumerate()
        .map(|(i, &secs)| {
            if i == app.preset_idx {
                format!("[{}s]", secs)
            } else {
                format!("{}s", secs)
            }
        })
        .collect();

    let header_text = format!(
        " Time: {}s | WPM: {:.0} | Mode: {} (Ctrl+D) | List: [{}] (Ctrl+W) | Tab: Restart | Esc: Quit ",
        time_left,
        wpm,
        preset_display.join("/"),
        wordlist_name
    );

    let header = Paragraph::new(header_text)
        .block(Block::default().borders(Borders::ALL))
        .style(Style::default().fg(Color::Yellow));
    f.render_widget(header, chunks[0]);

    let target_chars: Vec<char> = app.target_words.chars().collect();
    let input_chars: Vec<char> = app.user_input.chars().collect();

    let style_correct = Style::default().fg(Color::Green);
    let style_incorrect = Style::default().fg(Color::Red).bg(Color::Rgb(80, 0, 0));
    let style_untyped = Style::default().fg(Color::DarkGray);
    let style_active = Style::default()
        .fg(Color::Black)
        .bg(Color::White)
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
                .padding(Padding::new(1, 1, 1, 1)),
        )
        .wrap(Wrap { trim: false });
    f.render_widget(typing_area, chunks[1]);
}

fn render_finish_ui(f: &mut ratatui::Frame, app: &App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .margin(2)
        .constraints([Constraint::Length(9), Constraint::Min(5)])
        .split(f.area());

    if let Some(stats) = &app.last_run {
        let summary_lines = vec![
            Line::from(vec![
                Span::styled(" Net WPM:  ", Style::default()),
                Span::styled(
                    format!("{:.1}", stats.wpm),
                    Style::default().add_modifier(Modifier::BOLD),
                ),
            ]),
            Line::from(vec![
                Span::styled(" Raw WPM:  ", Style::default()),
                Span::styled(format!("{:.1}", stats.raw_wpm), Style::default().add_modifier(Modifier::BOLD)),
            ]),
            Line::from(vec![
                Span::styled(" Accuracy: ", Style::default()),
                Span::styled(
                    format!("{:.1}% ({}/{})", stats.accuracy, stats.correct_chars, stats.total_chars),
                    Style::default().add_modifier(Modifier::BOLD),
                ),
            ]),
            Line::from(vec![
                Span::styled(" Time / List:  ", Style::default()),
                Span::styled(
                    format!("{:.1}s on [{}]", stats.duration_secs, stats.wordlist_name),
                    Style::default(),
                ),
            ]),
            Line::from(""),
            Line::from(Span::styled(
                " [Tab/Enter] Restart | [Ctrl+D] Timer (30/60/90s) | [Ctrl+W] Wordlist | [Esc] Quit",
                Style::default(),
            )),
        ];

        let summary_block = Paragraph::new(summary_lines).block(
            Block::default()
                .title(" Run Complete! ")
                .borders(Borders::ALL)
                .padding(Padding::new(1, 1, 1, 0)),
        );
        f.render_widget(summary_block, chunks[0]);
    }

    let header_cells = ["Run", "Net WPM", "Raw WPM", "Accuracy", "Time", "Wordlist"]
        .iter()
        .map(|h| Cell::from(*h).style(Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)));
    let header = Row::new(header_cells).height(1).bottom_margin(1);

    let total_runs = app.history.len();
    let rows = app.history.iter().enumerate().rev().take(10).map(|(idx, run)| {
        Row::new(vec![
            Cell::from(format!("#{}", idx + 1)),
            Cell::from(format!("{:.1}", run.wpm)),
            Cell::from(format!("{:.1}", run.raw_wpm)),
            Cell::from(format!("{:.1}%", run.accuracy)),
            Cell::from(format!("{:.1}s", run.duration_secs)),
            Cell::from(run.wordlist_name.clone()),
        ])
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
            .padding(Padding::new(1, 1, 1, 1)),
    );

    f.render_widget(table, chunks[1]);
}