use std::{
    io,
    path::PathBuf,
    sync::mpsc::{self, Sender},
    thread,
    time::Duration,
};

use anyhow::Result;
use crossterm::{
    event::{self, Event, KeyCode},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    text::{Span, Line},
    widgets::{Block, Borders, List, ListItem, Paragraph},
    Terminal,
};
use rodio::{Decoder, OutputStream, Sink};
use walkdir::WalkDir;

enum PlayerMessage {
    Play(PathBuf),
    Stop,
    Next,
    Previous,
    Quit,
    AddDirectory(PathBuf),
    RemoveDirectory(usize),
    SetVolume(f32),
}

struct MusicPlayer {
    songs: Vec<PathBuf>,
    current_index: usize,
    _player_tx: Sender<PlayerMessage>,
    is_playing: bool,
    music_dirs: Vec<PathBuf>,
    volume: f32,
}

impl MusicPlayer {
    fn new(music_dirs: &[PathBuf]) -> Result<Self> {
        let mut songs = Vec::new();
        for dir in music_dirs {
            for entry in WalkDir::new(dir).follow_links(true) {
                let entry = entry?;
                let path = entry.path();
                if let Some(ext) = path.extension() {
                    if ext == "mp3" || ext == "ogg" || ext == "flac" {
                        songs.push(path.to_owned());
                    }
                }
            }
        }

        let (tx, rx) = mpsc::channel();
        let _player_tx = tx.clone();

        // Audio playback thread
        thread::spawn(move || {
            let (_stream, stream_handle) = OutputStream::try_default().unwrap();
            let mut sink: Option<Sink> = None;
            let mut current_volume = 1.0;

            while let Ok(msg) = rx.recv() {
                match msg {
                    PlayerMessage::Play(path) => {
                        if let Some(s) = sink.take() {
                            s.stop();
                        }
                        if let Ok(file) = std::fs::File::open(&path) {
                            if let Ok(source) = Decoder::new(file) {
                                let new_sink = Sink::try_new(&stream_handle).unwrap();
                                new_sink.set_volume(current_volume);
                                new_sink.append(source);
                                new_sink.play();
                                sink = Some(new_sink);
                            }
                        }
                    }
                    PlayerMessage::SetVolume(vol) => {
                        current_volume = vol;
                        if let Some(s) = &sink {
                            s.set_volume(vol);
                        }
                    }
                    PlayerMessage::Stop => {
                        if let Some(s) = &sink {
                            s.stop();
                        }
                    }
                    PlayerMessage::Quit => break,
                    _ => {}
                }
            }
        });

        Ok(MusicPlayer {
            songs,
            current_index: 0,
            _player_tx: tx,
            is_playing: false,
            music_dirs: music_dirs.to_vec(),
            volume: 1.0,
        })
    }

    fn play_current(&mut self) {
        if let Some(path) = self.songs.get(self.current_index) {
            self._player_tx
                .send(PlayerMessage::Play(path.clone()))
                .unwrap();
            self.is_playing = true;
        }
    }

    fn stop(&mut self) {
        self._player_tx.send(PlayerMessage::Stop).unwrap();
        self.is_playing = false;
    }

    fn next(&mut self) {
        self.current_index = (self.current_index + 1) % self.songs.len();
        if self.is_playing {
            self.play_current();
        }
    }

    fn previous(&mut self) {
        if self.current_index > 0 {
            self.current_index -= 1;
        } else {
            self.current_index = self.songs.len() - 1;
        }
        if self.is_playing {
            self.play_current();
        }
    }

    fn add_directory(&mut self, new_dir: PathBuf) -> Result<()> {
        if !new_dir.exists() {
            return Err(anyhow::anyhow!("Directory does not exist"));
        }

        // Add new songs from the directory
        for entry in WalkDir::new(&new_dir).follow_links(true) {
            let entry = entry?;
            let path = entry.path();
            if let Some(ext) = path.extension() {
                if ext == "mp3" || ext == "ogg" || ext == "flac" {
                    self.songs.push(path.to_owned());
                }
            }
        }

        self.music_dirs.push(new_dir);
        Ok(())
    }

    fn remove_directory(&mut self, index: usize) -> Result<()> {
        if index >= self.music_dirs.len() {
            return Err(anyhow::anyhow!("Invalid directory index"));
        }

        let removed_dir = &self.music_dirs[index];
        self.songs.retain(|path| !path.starts_with(removed_dir));
        self.music_dirs.remove(index);

        // Reset current index if needed
        if self.current_index >= self.songs.len() {
            self.current_index = self.songs.len().saturating_sub(1);
        }

        Ok(())
    }

    fn set_volume(&mut self, delta: f32) {
        self.volume = (self.volume + delta).clamp(0.0, 1.0);
        self._player_tx.send(PlayerMessage::SetVolume(self.volume)).unwrap();
    }
}

struct App {
    player: MusicPlayer,
    command_mode: bool,
    command_input: String,
    message: Option<String>,
}

fn main() -> Result<()> {
    // Setup terminal
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let initial_dirs = vec![
        PathBuf::from("C:/Users/lintr/AppData/Roaming/Python/Python312/Scripts"),
    ];

    let mut app = App {
        player: MusicPlayer::new(&initial_dirs)?,
        command_mode: false,
        command_input: String::new(),
        message: None,
    };

    let mut scroll_offset = 0;
    let mut last_key_time = std::time::Instant::now();
    let key_delay = Duration::from_millis(150); // 150ms delay between key presses

    loop {
        terminal.draw(|f| {
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Length(3), // Title
                    Constraint::Min(0),    // Songs
                    Constraint::Length(3), // Directories
                    Constraint::Length(3), // Controls/Command
                ])
                .split(f.size());

            // Title
            let title = Paragraph::new("Music Player")
                .style(Style::default().fg(Color::Cyan))
                .block(Block::default().borders(Borders::ALL));
            f.render_widget(title, chunks[0]);

            // Songs list
            let songs: Vec<ListItem> = app.player
                .songs
                .iter()
                .enumerate()
                .map(|(i, path)| {
                    let style = if i == app.player.current_index {
                        Style::default()
                            .fg(Color::Green)
                            .add_modifier(Modifier::BOLD)
                    } else {
                        Style::default().fg(Color::White)
                    };
                    ListItem::new(path.file_name().unwrap().to_string_lossy()).style(style)
                })
                .collect();

            let songs_list = List::new(songs)
                .block(Block::default().borders(Borders::ALL).title(
                    format!("Songs (Volume: {}%)", (app.player.volume * 100.0) as i32)
                ));
            f.render_widget(songs_list, chunks[1]);

            // Directories list
            let dirs: Vec<ListItem> = app.player
                .music_dirs
                .iter()
                .enumerate()
                .map(|(i, path)| {
                    ListItem::new(format!("{}: {}", i, path.display()))
                        .style(Style::default().fg(Color::Yellow))
                })
                .collect();

            let dirs_list = List::new(dirs)
                .block(Block::default().borders(Borders::ALL).title("Directories"));
            f.render_widget(dirs_list, chunks[2]);

            // Bottom area - either controls or command input
            if app.command_mode {
                let input = Paragraph::new(format!("> {}", app.command_input))
                    .style(Style::default().fg(Color::Yellow))
                    .block(Block::default().borders(Borders::ALL));
                f.render_widget(input, chunks[3]);
            } else {
                let controls = Paragraph::new(vec![
                    Line::from(vec![
                        Span::raw("Space: Play/Pause | "),
                        Span::raw("n: Next | "),
                        Span::raw("p: Previous | "),
                        Span::raw("-/+: Volume | "),
                        Span::raw(":: Command | "),
                        Span::raw("q: Quit"),
                    ]),
                    Line::from(vec![
                        if let Some(msg) = &app.message {
                            Span::styled(msg, Style::default().fg(Color::Yellow))
                        } else {
                            Span::raw("")
                        }
                    ]),
                ])
                .block(Block::default().borders(Borders::ALL));
                f.render_widget(controls, chunks[3]);
            }
        })?;

        if event::poll(Duration::from_millis(100))? {
            if let Event::Key(key) = event::read()? {
                let now = std::time::Instant::now();
                if now.duration_since(last_key_time) < key_delay {
                    continue;
                }
                last_key_time = now;

                if app.command_mode {
                    match key.code {
                        KeyCode::Enter => {
                            let cmd = app.command_input.trim();
                            if cmd.starts_with("add ") {
                                let path = PathBuf::from(cmd.trim_start_matches("add "));
                                match app.player.add_directory(path) {
                                    Ok(_) => app.message = Some("Directory added successfully".to_string()),
                                    Err(e) => app.message = Some(format!("Error: {}", e)),
                                }
                            } else if cmd.starts_with("remove ") {
                                if let Ok(index) = cmd.trim_start_matches("remove ").parse::<usize>() {
                                    match app.player.remove_directory(index) {
                                        Ok(_) => app.message = Some("Directory removed successfully".to_string()),
                                        Err(e) => app.message = Some(format!("Error: {}", e)),
                                    }
                                }
                            }
                            app.command_mode = false;
                            app.command_input.clear();
                        }
                        KeyCode::Esc => {
                            app.command_mode = false;
                            app.command_input.clear();
                        }
                        KeyCode::Char(c) => {
                            app.command_input.push(c);
                        }
                        KeyCode::Backspace => {
                            app.command_input.pop();
                        }
                        _ => {}
                    }
                } else {
                    match key.code {
                        KeyCode::Char('q') => {
                            app.player._player_tx.send(PlayerMessage::Quit)?;
                            break;
                        }
                        KeyCode::Char(' ') => {
                            if app.player.is_playing {
                                app.player.stop();
                            } else {
                                app.player.play_current();
                            }
                        }
                        KeyCode::Char('n') => {
                            app.player.next();
                            if app.player.current_index as i32 - scroll_offset >= 10 {
                                scroll_offset = app.player.current_index as i32 - 5;
                            }
                        },
                        KeyCode::Char('p') => {
                            app.player.previous();
                            if app.player.current_index as i32 - scroll_offset < 0 {
                                scroll_offset = (app.player.current_index as i32).max(0);
                            }
                        },
                        KeyCode::Char('+') | KeyCode::Char('=') => app.player.set_volume(0.05),
                        KeyCode::Char('-') => app.player.set_volume(-0.05),
                        KeyCode::Char(':') => {
                            app.command_mode = true;
                            app.message = None;
                        }
                        _ => {}
                    }
                }
            }
        }
    }

    // Cleanup
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    Ok(())
}
