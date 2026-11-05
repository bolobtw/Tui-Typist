use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind},
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
    fs::{self, OpenOptions},
    io::{self, BufRead, BufReader, Write},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

const HISTORY_FILE: &str = "history.jsonl";

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
    timestamp: u64,
}

struct App {
    state: AppState,
    target_words: String,
    user_input: String,
    start_time: Option<Instant>,
    time_limit: Duration,
    total_keystrokes: usize,
    correct_keystrokes: usize,
    last_run: Option<RunStats>,
    history: Vec<RunStats>,
}

impl App {
    fn new() -> App {
        let mut app = App {
            state: AppState::Typing,
            target_words: String::new(),
            user_input: String::new(),
            start_time: None,
            time_limit: Duration::from_secs(30),
            total_keystrokes: 0,
            correct_keystrokes: 0,
            last_run: None,
            history: load_history(),
        };
        app.reset_run();
        app
    }

    fn reset_run(&mut self) {
        let words_str = include_str!("../words.txt");
        let mut words: Vec<&str> = words_str.split_whitespace().collect();
        let mut rng = rand::rng();
        words.shuffle(&mut rng);

        let take_count = words.len().min(50);
        self.target_words = words[..take_count].join(" ");
        self.user_input.clear();
        self.start_time = None;
        self.total_keystrokes = 0;
        self.correct_keystrokes = 0;
        self.state = AppState::Typing;
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
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let mut app = App::new();
    let res = run_app(&mut terminal, &mut app);

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    if let Err(err) = res {
        println!("{:?}", err)
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

                match key.code {
                    KeyCode::Esc => return Ok(()),
                    KeyCode::Tab => {
                        app.reset_run();
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

    let header_text = format!(
        " Time Remaining: {}s | WPM: {:.0} | Tab: Restart | Esc: Quit ",
        time_left, wpm
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
                .padding(Padding::new(1, 1, 0, 0)),
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
                    Style::default().fg(Color::Green).add_modifier(Modifier::BOLD),
                ),
            ]),
            Line::from(vec![
                Span::styled(" Raw WPM:  ", Style::default()),
                Span::styled(format!("{:.1}", stats.raw_wpm), Style::default()),
            ]),
            Line::from(vec![
                Span::styled(" Accuracy: ", Style::default()),
                Span::styled(
                    format!("{:.1}% ({}/{})", stats.accuracy, stats.correct_chars, stats.total_chars),
                    Style::default(),
                ),
            ]),
            Line::from(vec![
                Span::styled(" Time:     ", Style::default()),
                Span::styled(format!("{:.1}s", stats.duration_secs), Style::default()),
            ]),
            Line::from(""),
            Line::from(Span::styled(
                " Press [Tab] or [Enter] to play again | [Esc] Quit",
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

    let header_cells = ["Run", "Net WPM", "Raw WPM", "Accuracy", "Time"]
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
        ])
    });

    let table = Table::new(
        rows,
        [
            Constraint::Length(6),
            Constraint::Length(10),
            Constraint::Length(10),
            Constraint::Length(10),
            Constraint::Min(8),
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