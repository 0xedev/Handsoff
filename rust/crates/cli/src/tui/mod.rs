pub mod agents;
pub mod events;
pub mod timeline;

use crossterm::{
    event::{self, Event, KeyCode},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{backend::CrosstermBackend, Terminal};
use std::io;
use std::time::Duration;

#[derive(PartialEq)]
enum View {
    Agents,
    Timeline,
}

pub async fn run(daemon_url: &str) -> anyhow::Result<()> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let result = event_loop(&mut terminal, daemon_url).await;

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    result
}

async fn event_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    daemon_url: &str,
) -> anyhow::Result<()> {
    let mut view = View::Agents;

    loop {
        match view {
            View::Agents => {
                let data = agents::fetch(daemon_url).await.unwrap_or_default();
                let handoffs = events::fetch_handoffs(daemon_url).await.unwrap_or_default();
                terminal.draw(|frame| {
                    agents::render(frame, &data, &handoffs);
                })?;
            }
            View::Timeline => {
                let handoffs = timeline::fetch(daemon_url).await.unwrap_or_default();
                terminal.draw(|frame| {
                    timeline::render(frame, &handoffs);
                })?;
            }
        }

        if event::poll(Duration::from_millis(500))? {
            if let Event::Key(key) = event::read()? {
                match key.code {
                    KeyCode::Char('q') | KeyCode::Char('Q') => break,
                    KeyCode::Tab => {
                        view = if view == View::Agents {
                            View::Timeline
                        } else {
                            View::Agents
                        };
                    }
                    _ => {}
                }
            }
        }
    }
    Ok(())
}
