use amara_core::peer::PeerInfo;
use crossterm::{
    event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::{Backend, CrosstermBackend},
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph},
    Frame, Terminal,
};
use std::io;
use std::time::Duration;
use tokio::sync::mpsc;

pub struct TuiApp {
    pub peers: Vec<PeerInfo>,
    pub list_state: ListState,
    pub status_message: String,
    pub logs: Vec<String>,
}

impl TuiApp {
    pub fn new() -> Self {
        Self {
            peers: Vec::new(),
            list_state: ListState::default(),
            status_message: "Starting AMARA Node...".to_string(),
            logs: Vec::new(),
        }
    }

    pub fn next(&mut self) {
        let i = match self.list_state.selected() {
            Some(i) => {
                if i >= self.peers.len().saturating_sub(1) {
                    0
                } else {
                    i + 1
                }
            }
            None => 0,
        };
        self.list_state.select(Some(i));
    }

    pub fn previous(&mut self) {
        let i = match self.list_state.selected() {
            Some(i) => {
                if i == 0 {
                    self.peers.len().saturating_sub(1)
                } else {
                    i - 1
                }
            }
            None => 0,
        };
        self.list_state.select(Some(i));
    }
}

pub async fn run_tui(mut app: TuiApp, mut rx: mpsc::Receiver<PeerInfo>) -> anyhow::Result<()> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    loop {
        // Handle incoming mesh updates
        while let Ok(peer) = rx.try_recv() {
            if !app.peers.iter().any(|p| p.id == peer.id) {
                app.peers.push(peer);
                app.status_message = format!("Discovered peer: {}", app.peers.last().unwrap().name);
            }
        }

        terminal.draw(|f| draw_ui(f, &mut app))?;

        if event::poll(Duration::from_millis(100))? {
            if let Event::Key(key) = event::read()? {
                match key.code {
                    KeyCode::Char('q') | KeyCode::Esc => break,
                    KeyCode::Down | KeyCode::Char('j') => app.next(),
                    KeyCode::Up | KeyCode::Char('k') => app.previous(),
                    _ => {}
                }
            }
        }
    }

    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
    terminal.show_cursor()?;
    Ok(())
}

fn draw_ui<B: Backend>(f: &mut Frame<B>, app: &mut TuiApp) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .margin(1)
        .constraints(
            [
                Constraint::Length(3),
                Constraint::Min(0),
                Constraint::Length(3),
            ]
            .as_ref(),
        )
        .split(f.size());

    // Title Block
    let title = Paragraph::new(Line::from(vec![
        Span::styled(" AMARA ", Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)),
        Span::raw(" Decentralized Proximity Network for AI Agents "),
    ]))
    .block(Block::default().borders(Borders::ALL).title("Status"));
    f.render_widget(title, chunks[0]);

    // Main Content
    let content_chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(40), Constraint::Percentage(60)].as_ref())
        .split(chunks[1]);

    // Peer List
    let items: Vec<ListItem> = app
        .peers
        .iter()
        .map(|p| {
            let caps = p.capabilities.join(", ");
            let content = vec![
                Line::from(Span::styled(&p.name, Style::default().fg(Color::Green))),
                Line::from(Span::styled(format!("Caps: {}", caps), Style::default().fg(Color::DarkGray))),
            ];
            ListItem::new(content)
        })
        .collect();

    let peers_list = List::new(items)
        .block(Block::default().borders(Borders::ALL).title("Nearby Agents"))
        .highlight_style(
            Style::default()
                .bg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol(">> ");
    f.render_stateful_widget(peers_list, content_chunks[0], &mut app.list_state);

    // Selected Peer Details
    let details_text = if let Some(i) = app.list_state.selected() {
        if let Some(peer) = app.peers.get(i) {
            format!(
                "Identity: {}\nName: {}\nPlatform: {:?}\nLoad: {:?}\nAvailability: {:?}\nCapabilities: {}\n\nPress 'Enter' to delegate task...",
                peer.id,
                peer.name,
                peer.platform,
                peer.load,
                peer.availability,
                peer.capabilities.join(", ")
            )
        } else {
            "Select an agent".to_string()
        }
    } else {
        "Select an agent".to_string()
    };

    let details = Paragraph::new(details_text)
        .block(Block::default().borders(Borders::ALL).title("Agent Details"));
    f.render_widget(details, content_chunks[1]);

    // Status Bar
    let status = Paragraph::new(app.status_message.clone())
        .block(Block::default().borders(Borders::ALL));
    f.render_widget(status, chunks[2]);
}
