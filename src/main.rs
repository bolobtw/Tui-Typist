use crossterm::{
    event::{self, Event, KeyCode},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use rand::seq::SliceRandom;
use ratatui::{
    Terminal,
    backend::{Backend, CrosstermBackend},
    layout::{Constraint, Direction, Layout},
    style::{Color, Style},
    text::{Line, Span, Text},
    widgets::{Block, Borders, Paragraph, Wrap, Padding},
};
use std::{
    io,
    time::{Duration, Instant},
};

struct App {
    target_words: String,
    user_input: String,
    start_time: Option<Instant>,
    time_limit: Duration,
}

impl App {
    fn new() -> App {
        let words_str = include_str!("../words.txt");
        let mut words: Vec<&str> = words_str.split_whitespace().collect();
        let mut rng = rand::rng();
        words.shuffle(&mut rng);

        let take_count = words.len().min(50);

        App {
            target_words: words[..take_count].join(" "),
            user_input: String::new(),
            start_time: None,
            time_limit: Duration::from_secs(30),
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

        if event::poll(Duration::from_millis(100))? {
            if let Event::Key(key) = event::read()? {
                match key.code {
                    KeyCode::Esc => return Ok(()),
                    KeyCode::Backspace => {
                        app.user_input.pop();
                    }
                    KeyCode::Char(c) => {
                        if app.user_input.len() < app.target_words.len() {
                            if app.start_time.is_none() {
                                app.start_time = Some(Instant::now());
                            }
                            app.user_input.push(c);
                        }
                    }
                    _ => {}
                }
            }
        }

        if let Some(start) = app.start_time {
            if start.elapsed() >= app.time_limit {
                return Ok(());
            }
        }
    }
}

fn ui(f: &mut ratatui::Frame, app: &App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .margin(2)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(1),
        ])
        .split(f.area());

    let time_left = if let Some(start) = app.start_time {
        let elapsed = start.elapsed().as_secs();
        app.time_limit.as_secs().saturating_sub(elapsed)
    } else {
        app.time_limit.as_secs()
    };

    let header_text = format!(" Time Remaining: {}s | Press 'Esc' to Quit ", time_left);
    let header = Paragraph::new(header_text)
        .block(Block::default().borders(Borders::ALL))
        .style(Style::default().fg(Color::Yellow));
    f.render_widget(header, chunks[0]);

    let target_chars: Vec<char> = app.target_words.chars().collect();
    let input_chars: Vec<char> = app.user_input.chars().collect();

    let style_correct = Style::default().fg(Color::Green);
    let style_incorrect = Style::default().fg(Color::Red).bg(Color::Rgb(80, 0, 0));
    let style_untyped = Style::default().fg(Color::DarkGray);
    let style_active = Style::default().fg(Color::Black).bg(Color::White).bold();

    let mut styled_spans = Vec::new();

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

    let colored_text = Line::from(styled_spans);

    let typing_area = Paragraph::new(Text::from(colored_text))
        .block(
            Block::default()
                .title(" Tui-Typist ")
                .borders(Borders::ALL)
                .padding(Padding::new(1, 1, 0, 0))
        )
        .wrap(Wrap { trim: false });
    f.render_widget(typing_area, chunks[1]);
}