use std::{
    io,
    path::PathBuf,
    sync::mpsc::{self, Sender},
    thread,
    time::Duration,
    collections::VecDeque,
    time::Instant,
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
    widgets::{Block, Borders, List, ListItem, Paragraph, ListState, Tabs},
    Terminal,
    prelude::Alignment,
};
use rodio::{Decoder, OutputStream, Sink};
use walkdir::WalkDir;
use rand::seq::SliceRandom;
use id3::{Tag, TagLike};

enum PlayerMessage {
    Play(PathBuf),
    Stop,
    Next,
    Previous,
    Quit,
    AddDirectory(PathBuf),
    RemoveDirectory(usize),
    SetVolume(f32),
    Shuffle,
    AddToQueue(usize),
}

#[derive(Clone)]
struct Song {
    path: PathBuf,
    title: String,
    artist: String,
    album: String,
    genre: String,
}

impl Song {
    fn new(path: PathBuf) -> Self {
        let filename = path.file_stem()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();
        
        // Split filename by " - " to get artist and title
        let parts: Vec<&str> = filename.splitn(2, " - ").collect();
        
        let (mut artist, mut title) = match parts.as_slice() {
            [artist, title] => {
                // Add spaces between multiple artists (separated by &, feat., or featuring)
                let artist = artist
                    .replace("&", " & ")
                    .replace("feat.", " feat. ")
                    .replace("featuring", " featuring ")
                    .replace("  ", " ") // Remove any double spaces
                    .trim()
                    .to_string();
                (artist, title.to_string())
            },
            _ => (String::from("Unknown Artist"), filename),
        };

        let mut album = String::from("Unknown Album");
        let mut genre = String::from("Unknown Genre");

        // Try to read metadata
        if let Ok(tag) = Tag::read_from_path(&path) {
            if let Some(meta_title) = tag.title() {
                title = meta_title.to_string();
            }
            if let Some(meta_artist) = tag.artist() {
                // Add spaces between multiple artists in metadata too
                artist = meta_artist
                    .replace("&", " & ")
                    .replace("feat.", " feat. ")
                    .replace("featuring", " featuring ")
                    .replace("  ", " ")
                    .trim()
                    .to_string();
            }
            if let Some(meta_album) = tag.album() {
                album = meta_album.to_string();
            }
            if let Some(meta_genre) = tag.genre() {
                genre = meta_genre.to_string();
            }
        }

        Song {
            path,
            title,
            artist,
            album,
            genre,
        }
    }
}

struct MusicPlayer {
    songs: Vec<Song>,
    current_index: usize,
    _player_tx: Sender<PlayerMessage>,
    is_playing: bool,
    music_dirs: Vec<PathBuf>,
    volume: f32,
    queue: VecDeque<usize>,
    view_mode: ViewMode,
    search_query: String,
}

#[derive(PartialEq)]
enum ViewMode {
    AllSongs,
    Artists,
    Albums,
    Genres,
    Queue,
    Search,
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
                        songs.push(Song::new(path.to_owned()));
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
            queue: VecDeque::new(),
            view_mode: ViewMode::AllSongs,
            search_query: String::new(),
        })
    }

    fn play_current(&mut self) {
        if let Some(song) = self.songs.get(self.current_index) {
            self._player_tx
                .send(PlayerMessage::Play(song.path.clone()))
                .unwrap();
            self.is_playing = true;
        }
    }

    fn stop(&mut self) {
        self._player_tx.send(PlayerMessage::Stop).unwrap();
        self.is_playing = false;
    }

    fn next(&mut self) {
        if let Some(next_index) = self.queue.pop_front() {
            self.current_index = next_index;
        } else {
            self.current_index = (self.current_index + 1) % self.songs.len();
        }
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
                    self.songs.push(Song::new(path.to_owned()));
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
        self.songs.retain(|song| !song.path.starts_with(removed_dir));
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

    fn shuffle(&mut self) {
        let mut rng = rand::thread_rng();
        self.songs.shuffle(&mut rng);
        self.current_index = 0;
        if self.is_playing {
            self.play_current();
        }
    }

    fn add_to_queue(&mut self, index: usize) {
        if index < self.songs.len() {
            self.queue.push_back(index);
        }
    }

    fn search(&mut self, query: &str) -> Vec<(usize, &Song)> {
        self.songs.iter().enumerate()
            .filter(|(_, song)| {
                song.title.to_lowercase().contains(&query.to_lowercase()) ||
                song.artist.to_lowercase().contains(&query.to_lowercase()) ||
                song.album.to_lowercase().contains(&query.to_lowercase())
            })
            .collect()
    }
}

struct App {
    player: MusicPlayer,
    command_mode: bool,
    command_input: String,
    message: Option<String>,
    search_mode: bool,
    search_input: String,
    selected_artist: Option<String>,
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
        search_mode: false,
        search_input: String::new(),
        selected_artist: None,
    };

    let mut scroll_offset = 0;
    let mut last_key_time = Instant::now();
    let key_delay = Duration::from_millis(150); // 150ms delay between key presses

    loop {
        terminal.draw(|f| {
            // Create a more complex layout
            let main_chunks = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([
                    Constraint::Percentage(70),
                    Constraint::Percentage(30),
                ])
                .split(f.size());

            let left_chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Length(3),  // Title
                    Constraint::Length(3),  // View mode tabs
                    Constraint::Min(0),     // Main content
                    Constraint::Length(3),  // Controls
                ])
                .split(main_chunks[0]);

            let right_chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Length(10), // Now Playing (increased height)
                    Constraint::Min(0),     // Queue
                ])
                .split(main_chunks[1]);

            // Render title
            let title = Paragraph::new("Music Player")
                .style(Style::default().fg(Color::Cyan))
                .block(Block::default().borders(Borders::ALL));
            f.render_widget(title, left_chunks[0]);

            // Render view mode tabs
            let view_modes = vec!["Songs", "Artists", "Albums", "Genres", "Queue", "Search"];
            let tabs = Tabs::new(view_modes)
                .select(match app.player.view_mode {
                    ViewMode::AllSongs => 0,
                    ViewMode::Artists => 1,
                    ViewMode::Albums => 2,
                    ViewMode::Genres => 3,
                    ViewMode::Queue => 4,
                    ViewMode::Search => 5,
                })
                .block(Block::default().borders(Borders::ALL))
                .style(Style::default().fg(Color::White))
                .highlight_style(Style::default().fg(Color::Cyan));
            f.render_widget(tabs, left_chunks[1]);

            // Render main content based on view mode
            let content: Vec<ListItem> = match app.player.view_mode {
                ViewMode::AllSongs => app.player.songs.iter().enumerate()
                    .map(|(i, song)| {
                        let style = if i == app.player.current_index {
                            Style::default().fg(Color::Green).add_modifier(Modifier::BOLD)
                        } else {
                            Style::default().fg(Color::White)
                        };
                        ListItem::new(song.title.clone()).style(style)
                    })
                    .collect(),
                ViewMode::Artists => {
                    if let Some(selected_artist) = &app.selected_artist {
                        // Show songs by selected artist
                        app.player.songs.iter().enumerate()
                            .filter(|(_, song)| &song.artist == selected_artist)
                            .map(|(i, song)| {
                                let style = if i == app.player.current_index {
                                    Style::default().fg(Color::Green).add_modifier(Modifier::BOLD)
                                } else {
                                    Style::default().fg(Color::White)
                                };
                                ListItem::new(song.title.clone()).style(style)
                            })
                            .collect()
                    } else {
                        // Show list of artists
                        let mut artists: Vec<_> = app.player.songs.iter()
                            .map(|song| &song.artist)
                            .collect();
                        artists.sort();
                        artists.dedup();
                        artists.into_iter()
                            .map(|artist| ListItem::new(artist.to_string()))
                            .collect()
                    }
                },
                ViewMode::Albums => {
                    let mut albums: Vec<_> = app.player.songs.iter()
                        .map(|song| (song.album.as_str(), song.artist.as_str()))
                        .collect();
                    albums.sort();
                    albums.dedup();
                    albums.into_iter()
                        .map(|(album, artist)| {
                            ListItem::new(format!("{} (by {})", album, artist))
                        })
                        .collect()
                },
                ViewMode::Genres => {
                    let mut genres: Vec<_> = app.player.songs.iter()
                        .map(|song| song.genre.as_str())
                        .collect();
                    genres.sort();
                    genres.dedup();
                    genres.into_iter()
                        .map(|genre| ListItem::new(genre.to_string()))
                        .collect()
                },
                ViewMode::Queue => app.player.queue.iter()
                    .map(|&index| {
                        let song = &app.player.songs[index];
                        ListItem::new(format!("{} - {}", song.artist, song.title))
                    })
                    .collect(),
                ViewMode::Search => {
                    if !app.search_input.is_empty() {
                        let current_index = app.player.current_index;
                        app.player.search(&app.search_input)
                            .into_iter()
                            .map(|(i, song)| {
                                let style = if i == current_index {
                                    Style::default().fg(Color::Green).add_modifier(Modifier::BOLD)
                                } else {
                                    Style::default().fg(Color::White)
                                };
                                ListItem::new(format!("{} - {}", song.artist, song.title)).style(style)
                            })
                            .collect()
                    } else {
                        vec![]
                    }
                },
            };

            // Clear the main content area before rendering the list
            let clear_block = Block::default()
                .borders(Borders::ALL)
                .title("Songs");
            f.render_widget(clear_block, left_chunks[2]);

            // Render the list with proper styling
            let content_list = List::new(content)
                .block(Block::default().borders(Borders::ALL))
                .highlight_style(Style::default().add_modifier(Modifier::BOLD))
                .highlight_symbol(">> ");

            let mut state = ListState::default();
            state.select(Some(scroll_offset));
            f.render_stateful_widget(content_list, left_chunks[2], &mut state);

            // Render Now Playing with proper formatting
            let now_playing = if let Some(song) = app.player.songs.get(app.player.current_index) {
                vec![
                    Line::from(""),
                    //Line::from(vec![Span::raw("Now Playing:")]),
                    //Line::from(""),
                    Line::from(vec![Span::raw(format!("Title: {}", song.title))]),
                    Line::from(vec![Span::raw(format!("Artist: {}", song.artist))]),
                    Line::from(vec![Span::raw(format!("Album: {}", song.album))]),
                    Line::from(vec![Span::raw(format!("Genre: {}", song.genre))]),
                    Line::from(""),
                    Line::from(vec![Span::raw(format!("Status: {}", 
                        if app.player.is_playing { "Playing" } else { "Paused" }
                    ))]),
                ]
            } else {
                vec![
                    Line::from(""),
                    Line::from(vec![Span::raw("Nothing playing")]),
                ]
            };

            let now_playing_widget = Paragraph::new(now_playing)
                .block(Block::default().borders(Borders::ALL).title("Now Playing"))
                .style(Style::default().fg(Color::Green))
                .alignment(Alignment::Left);
            f.render_widget(now_playing_widget, right_chunks[0]);

            // Render Queue
            let queue_items: Vec<ListItem> = app.player.queue.iter()
                .map(|&index| {
                    let song = &app.player.songs[index];
                    ListItem::new(format!("{} - {}", song.artist, song.title))
                })
                .collect();

            let queue_list = List::new(queue_items)
                .block(Block::default().borders(Borders::ALL).title("Queue"));
            f.render_widget(queue_list, right_chunks[1]);

            // Render controls
            let controls = if app.search_mode {
                Paragraph::new(format!("Search: {} (ESC to stop typing)", app.search_input))
            } else {
                Paragraph::new(vec![
                    Line::from(vec![
                        Span::raw("p: Play/Pause | "),
                        Span::raw("h/l: Prev/Next | "),
                        Span::raw("j/k: Move | "),
                        Span::raw("-/+: Volume | "),
                        Span::raw("s: Shuffle | "),
                        Span::raw("a: Add to Queue | "),
                        Span::raw("/: Search | "),
                        Span::raw("Space: Select | "),
                        Span::raw("Tab: Change View | "),
                        Span::raw("q: Quit"),
                    ])
                ])
            };
            f.render_widget(controls.block(Block::default().borders(Borders::ALL)), left_chunks[3]);
        })?;

        if event::poll(Duration::from_millis(100))? {
            if let Event::Key(key) = event::read()? {
                let now = Instant::now();
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
                        KeyCode::Char('q') if !app.search_mode => {
                            app.player._player_tx.send(PlayerMessage::Quit)?;
                            break;
                        },
                        KeyCode::Char('s') if !app.search_mode => {
                            app.player.shuffle();
                        },
                        KeyCode::Char('a') if !app.search_mode => {
                            app.player.add_to_queue(scroll_offset);
                            app.message = Some("Added to queue".to_string());
                        },
                        KeyCode::Char('p') if !app.search_mode => {
                            if app.player.is_playing {
                                app.player.stop();
                            } else {
                                app.player.play_current();
                            }
                        },
                        KeyCode::Char('j') if !app.search_mode => {
                            if scroll_offset < app.player.songs.len().saturating_sub(1) {
                                scroll_offset += 1;
                            }
                        },
                        KeyCode::Char('k') if !app.search_mode => {
                            if scroll_offset > 0 {
                                scroll_offset -= 1;
                            }
                        },
                        KeyCode::Char('h') if !app.search_mode => {
                            app.player.previous();
                            if app.player.current_index < scroll_offset {
                                scroll_offset = app.player.current_index;
                            }
                        },
                        KeyCode::Char('l') if !app.search_mode => {
                            app.player.next();
                            if app.player.current_index > scroll_offset {
                                scroll_offset = app.player.current_index;
                            }
                        },
                        KeyCode::Char(' ') if !app.search_mode => {
                            match app.player.view_mode {
                                ViewMode::Artists => {
                                    if app.selected_artist.is_none() {
                                        // Select artist
                                        if let Some(artist) = app.player.songs.iter()
                                            .map(|song| &song.artist)
                                            .collect::<Vec<_>>()
                                            .into_iter()
                                            .nth(scroll_offset) {
                                            app.selected_artist = Some(artist.to_string());
                                            scroll_offset = 0;  // Reset scroll position for song list
                                        }
                                    } else {
                                        // Select song from artist's songs
                                        if let Some(selected_artist) = &app.selected_artist {
                                            if let Some((index, _)) = app.player.songs.iter().enumerate()
                                                .filter(|(_, song)| &song.artist == selected_artist)
                                                .nth(scroll_offset) {
                                                app.player.current_index = index;
                                                app.player.play_current();
                                            }
                                        }
                                    }
                                },
                                _ => {
                                    app.player.current_index = scroll_offset;
                                    app.player.play_current();
                                }
                            }
                        },
                        KeyCode::Tab if !app.search_mode => {
                            app.player.view_mode = match app.player.view_mode {
                                ViewMode::AllSongs => ViewMode::Artists,
                                ViewMode::Artists => ViewMode::Albums,
                                ViewMode::Albums => ViewMode::Genres,
                                ViewMode::Genres => ViewMode::Queue,
                                ViewMode::Queue => ViewMode::Search,
                                ViewMode::Search => ViewMode::AllSongs,
                            };
                        },
                        KeyCode::Char('/') if !app.search_mode => {
                            app.search_mode = true;
                            app.player.view_mode = ViewMode::Search;
                        },
                        KeyCode::Esc => {
                            if app.search_mode {
                                app.search_mode = false;
                            } else if app.player.view_mode == ViewMode::Search {
                                app.search_input.clear();
                                app.player.view_mode = ViewMode::AllSongs;
                            } else if app.player.view_mode == ViewMode::Artists && app.selected_artist.is_some() {
                                app.selected_artist = None;
                                scroll_offset = 0;
                            }
                        },
                        KeyCode::Char(c) if app.search_mode => {
                            app.search_input.push(c);
                        },
                        KeyCode::Backspace if app.search_mode => {
                            app.search_input.pop();
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
