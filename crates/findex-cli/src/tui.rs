use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use crossterm::ExecutableCommand;
use findex_core::graph_query::query_graph;
use findex_core::intelligence::{graph_snapshot, impact_analysis, GraphSnapshot, ImpactReport};
use findex_core::runtime::{profile, RuntimeProfile};
use findex_core::search::local_embedder::create_embedder;
use findex_core::search::rerank::{create_reranker, Reranker};
use findex_core::search::vector::Embedder;
use findex_core::search_codebase_with_components;
use findex_core::storage::{Storage, Symbol};
use findex_core::updater::{AvailableUpdate, UpdateCheck};
use findex_core::{ingest_codebase, IngestionStats};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Layout, Rect, Size};
use ratatui::style::{Color, Modifier, Style};
use ratatui::symbols::Marker;
use ratatui::text::{Line, Span};
use ratatui::widgets::canvas::{Canvas, Line as CanvasLine};
use ratatui::widgets::{
    Block, BorderType, Borders, Clear, Gauge, List, ListItem, ListState, Paragraph, Sparkline, Wrap,
};
use ratatui::{Frame, Terminal};
use ratatui_image::{
    picker::Picker, protocol::Protocol, Image as TerminalImage, Resize as ImageResize,
};
use ratatui_textarea::TextArea;
use ratatui_toaster::{ToastBuilder, ToastEngine, ToastEngineBuilder, ToastPosition, ToastType};
use std::collections::{HashMap, VecDeque};
use std::f64::consts::TAU;
use std::io;
use std::path::PathBuf;
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};
use syntect::easy::HighlightLines;
use syntect::highlighting::{FontStyle, ThemeSet};
use syntect::parsing::SyntaxSet;
use syntect::util::LinesWithEndings;
use tachyonfx::{fx, EffectManager, Interpolation};
use tui_big_text::{BigText, PixelSize};
use tui_logger::{TuiLoggerLevelOutput, TuiLoggerWidget};
use tui_overlay::{Backdrop, Overlay, OverlayState, Slide};
use tui_scrollview::{ScrollView, ScrollViewState};
use tui_tabs::TabNav;
use tui_tree_widget::{Tree, TreeItem, TreeState};

mod nord {
    use ratatui::style::Color;
    pub const BG: Color = Color::Rgb(46, 52, 64); // nord0
    pub const PANEL: Color = Color::Rgb(59, 66, 82); // nord1
    pub const PANEL_ALT: Color = Color::Rgb(67, 76, 94); // nord2
    pub const BORDER: Color = Color::Rgb(76, 86, 106); // nord3
    pub const TEXT: Color = Color::Rgb(216, 222, 233); // nord4
    pub const BRIGHT: Color = Color::Rgb(236, 239, 244); // nord6
    pub const CYAN: Color = Color::Rgb(136, 192, 208); // nord8
    pub const BLUE: Color = Color::Rgb(129, 161, 193); // nord9
    pub const GREEN: Color = Color::Rgb(163, 190, 140); // nord14
    pub const RED: Color = Color::Rgb(191, 97, 106); // nord11
    pub const YELLOW: Color = Color::Rgb(235, 203, 139); // nord13
    pub const PURPLE: Color = Color::Rgb(180, 142, 173); // nord15

    /// Theme token lookup with an automatic ANSI-256 downgrade for terminals
    /// that do not advertise true color.
    pub fn token(name: &str) -> Color {
        let theme = opaline::load_by_name("nord").unwrap_or_default();
        let color = theme.color(name);
        let true_color = std::env::var("COLORTERM")
            .map(|value| value.to_ascii_lowercase().contains("truecolor"))
            .unwrap_or(false)
            || std::env::var_os("WT_SESSION").is_some();
        if true_color {
            Color::Rgb(color.r, color.g, color.b)
        } else {
            Color::Indexed(coolor::Rgb::new(color.r, color.g, color.b).to_ansi().code)
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(usize)]
enum View {
    Dashboard,
    Search,
    Graph,
    Query,
    Inspector,
    Runtime,
}

struct IndexingJob {
    receiver: mpsc::Receiver<Result<IngestionStats, String>>,
    started: Instant,
}

struct UpdateJob {
    receiver: mpsc::Receiver<Result<UpdateJobResult, String>>,
}

enum UpdateJobResult {
    Checked(UpdateCheck),
    Installed(String),
}

impl View {
    const ALL: [Self; 6] = [
        Self::Dashboard,
        Self::Search,
        Self::Graph,
        Self::Query,
        Self::Inspector,
        Self::Runtime,
    ];

    fn next(self) -> Self {
        Self::ALL[(self as usize + 1) % Self::ALL.len()]
    }
}

pub struct App {
    view: View,
    db_path: std::path::PathBuf,
    index_root: Option<PathBuf>,
    indexing: Option<IndexingJob>,
    update_job: Option<UpdateJob>,
    available_update: Option<AvailableUpdate>,
    update_prompt: bool,
    symbols: Vec<Symbol>,
    files: usize,
    edges: usize,
    graph: GraphSnapshot,
    runtime: RuntimeProfile,
    search_input: TextArea<'static>,
    search_mode: &'static str,
    search_results: Vec<(Symbol, f32)>,
    search_state: ListState,
    search_dirty_at: Option<Instant>,
    query_input: TextArea<'static>,
    query_result: String,
    inspector: String,
    impact_report: Option<ImpactReport>,
    impact_tree: TreeState<String>,
    source_preview: Vec<Line<'static>>,
    query_scroll: ScrollViewState,
    logo: Option<Protocol>,
    message: String,
    tick: u64,
    motion: bool,
    nerd_icons: bool,
    help: OverlayState,
    effects: EffectManager<&'static str>,
    toasts: ToastEngine<()>,
    toast_until: Option<Instant>,
    memory_history: VecDeque<u64>,
    last_profile: Instant,
    reranker: Arc<dyn Reranker>,
    embedder: Arc<dyn Embedder>,
}

impl App {
    pub fn new(db_path: std::path::PathBuf) -> anyhow::Result<Self> {
        static LOGGER: std::sync::Once = std::sync::Once::new();
        LOGGER.call_once(|| {
            let _ = tui_logger::init_logger(log::LevelFilter::Info);
            tui_logger::set_default_level(log::LevelFilter::Info);
        });
        let storage = Storage::open(&db_path)?;
        let symbols = storage.list_symbols()?;
        let files = storage.list_files()?.len();
        let edges = storage.list_edges()?.len();
        let graph = graph_snapshot(&storage, 220)?;
        let index_root = storage
            .get_metadata::<String>("index:root")?
            .map(PathBuf::from);
        let runtime = profile(true);
        let process_memory_mib = runtime.process_memory_bytes / 1_048_576;
        let mut search_state = ListState::default();
        search_state.select(Some(0));
        let mut search_input = TextArea::default();
        search_input.set_placeholder_text("Find behavior, symbol, endpoint, or relationship");
        search_input.set_cursor_line_style(Style::default());
        let mut query_input = TextArea::new(vec![
            "MATCH (a)-[:Calls]->(b) RETURN a, b LIMIT 25".to_string()
        ]);
        query_input.set_cursor_line_style(Style::default());
        let reranker = create_reranker();
        let embedder = create_embedder(128);
        findex_core::runtime::start_model_idle_janitor(&embedder, &reranker);
        let update_job = start_update_check();
        Ok(Self {
            view: View::Dashboard,
            db_path,
            index_root,
            indexing: None,
            update_job,
            available_update: None,
            update_prompt: false,
            symbols,
            files,
            edges,
            graph,
            runtime,
            search_input,
            search_mode: "hybrid",
            search_results: Vec::new(),
            search_state,
            search_dirty_at: None,
            query_input,
            query_result: "Enter a Cypher-like graph query, then press Enter.".to_string(),
            inspector: "Select a search result and press Enter to inspect its blast radius."
                .to_string(),
            impact_report: None,
            impact_tree: TreeState::default(),
            source_preview: Vec::new(),
            query_scroll: ScrollViewState::new(),
            logo: None,
            message: "index ready".to_string(),
            tick: 0,
            motion: std::env::var("FINDEX_TUI_MOTION").as_deref() != Ok("0"),
            nerd_icons: std::env::var("FINDEX_TUI_ICONS").as_deref() != Ok("ascii"),
            help: OverlayState::new()
                .with_duration(Duration::from_millis(140))
                .with_easing(tui_overlay::Easing::EaseOut),
            effects: EffectManager::default(),
            toasts: ToastEngineBuilder::new(Rect::default())
                .default_duration(Duration::from_secs(3))
                .build(),
            toast_until: None,
            memory_history: VecDeque::from([process_memory_mib; 30]),
            last_profile: Instant::now(),
            reranker,
            embedder,
        })
    }

    pub fn run(&mut self) -> anyhow::Result<()> {
        enable_raw_mode()?;
        let mut stdout = io::stdout();
        stdout.execute(EnterAlternateScreen)?;
        let mut terminal = Terminal::new(CrosstermBackend::new(stdout))?;
        self.logo = load_terminal_logo();
        terminal.clear()?;
        let result = self.main_loop(&mut terminal);
        disable_raw_mode()?;
        terminal.backend_mut().execute(LeaveAlternateScreen)?;
        terminal.show_cursor()?;
        result
    }

    fn main_loop(
        &mut self,
        terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    ) -> anyhow::Result<()> {
        let storage = Storage::open(&self.db_path)?;
        let mut last_frame = Instant::now();
        loop {
            terminal.draw(|frame| self.draw(frame))?;
            if event::poll(Duration::from_millis(80))? {
                if let Event::Key(key) = event::read()? {
                    if key.kind == KeyEventKind::Press && self.handle_key(key, &storage)? {
                        return Ok(());
                    }
                }
            }
            let elapsed = last_frame.elapsed();
            last_frame = Instant::now();
            self.help.tick(elapsed);
            if self
                .toast_until
                .is_some_and(|until| Instant::now() >= until)
            {
                self.toasts.hide_toast();
                self.toast_until = None;
            }
            self.tick = self.tick.wrapping_add(1);
            self.poll_indexing(&storage)?;
            self.poll_update();
            if self
                .search_dirty_at
                .is_some_and(|started| started.elapsed() >= Duration::from_millis(180))
            {
                self.run_search(&storage)?;
            }
            if self.last_profile.elapsed() >= Duration::from_secs(1) {
                self.runtime = profile(self.view == View::Runtime);
                self.memory_history
                    .push_back(self.runtime.process_memory_bytes / 1_048_576);
                while self.memory_history.len() > 60 {
                    self.memory_history.pop_front();
                }
                self.last_profile = Instant::now();
            }
        }
    }

    fn handle_key(&mut self, key: KeyEvent, storage: &Storage) -> anyhow::Result<bool> {
        if self.update_prompt {
            match key.code {
                KeyCode::Enter => self.start_update_install(),
                KeyCode::Esc => self.update_prompt = false,
                _ => {}
            }
            return Ok(false);
        }
        if self.help.is_open() {
            self.help.close();
            return Ok(false);
        }
        if key.modifiers.contains(KeyModifiers::CONTROL)
            && matches!(key.code, KeyCode::Char('c' | 'q'))
        {
            return Ok(true);
        }
        if key.code == KeyCode::Char('?') {
            self.help.open();
            return Ok(false);
        }
        if key.code == KeyCode::F(8) {
            self.update_prompt = self.available_update.is_some();
            if !self.update_prompt {
                self.notify("no update is currently available", ToastType::Info);
            }
            return Ok(false);
        }
        if key.code == KeyCode::Tab {
            self.change_view(self.view.next());
            return Ok(false);
        }

        // Text views own ordinary character keys. This prevents digits and `r`
        // from unexpectedly switching views or starting work while typing.
        if self.view == View::Search {
            match key.code {
                KeyCode::F(2) => {
                    self.search_mode = match self.search_mode {
                        "hybrid" => "lexical",
                        "lexical" => "semantic",
                        _ => "hybrid",
                    };
                    self.search_dirty_at = Some(Instant::now());
                }
                KeyCode::Enter => {
                    if self.search_dirty_at.is_some() {
                        self.run_search(storage)?;
                    } else {
                        self.inspect_selected(storage)?;
                    }
                }
                KeyCode::Down => self.move_search(1),
                KeyCode::Up => self.move_search(-1),
                _ => {
                    if self.search_input.input(key) {
                        self.search_dirty_at = Some(Instant::now());
                    }
                }
            }
            return Ok(false);
        }
        if self.view == View::Query {
            if key.code == KeyCode::PageDown {
                self.query_scroll.scroll_page_down();
            } else if key.code == KeyCode::PageUp {
                self.query_scroll.scroll_page_up();
            } else if key.code == KeyCode::Enter {
                let query = self.query_input.lines().join("\n");
                self.query_result = match query_graph(storage, &query) {
                    Ok(result) => result.to_text(),
                    Err(error) => format!("query error: {error}"),
                };
                self.query_scroll.scroll_to_top();
                self.message = "graph query executed".to_string();
            } else {
                self.query_input.input(key);
            }
            return Ok(false);
        }

        match key.code {
            KeyCode::Esc => return Ok(true),
            KeyCode::Char('1') => self.change_view(View::Dashboard),
            KeyCode::Char('2') | KeyCode::Char('/') => self.change_view(View::Search),
            KeyCode::Char('3') => self.change_view(View::Graph),
            KeyCode::Char('4') => self.change_view(View::Query),
            KeyCode::Char('5') => self.change_view(View::Inspector),
            KeyCode::Char('6') => self.change_view(View::Runtime),
            KeyCode::Char('r') if self.view == View::Dashboard => self.start_reindex(),
            KeyCode::Char('r') if self.view == View::Runtime => {
                self.runtime = profile(true);
                self.message = "runtime probes refreshed".to_string();
            }
            KeyCode::Char('u') if self.available_update.is_some() => {
                self.update_prompt = true;
            }
            _ => {}
        }
        Ok(false)
    }

    fn change_view(&mut self, view: View) {
        if self.view == view {
            return;
        }
        self.view = view;
        if self.motion {
            self.effects.add_unique_effect(
                "view",
                fx::fade_from_fg(nord::BG, (140, Interpolation::CubicOut)),
            );
        }
    }

    fn notify(&mut self, message: impl Into<String>, toast_type: ToastType) {
        let message = message.into();
        log::info!(target: "findex::tui", "{message}");
        self.toasts.show_toast(
            ToastBuilder::new(message.into())
                .toast_type(toast_type)
                .position(ToastPosition::BottomRight),
        );
        self.toast_until = Some(Instant::now() + Duration::from_secs(3));
    }

    fn start_reindex(&mut self) {
        if self.indexing.is_some() {
            self.message = "reindex already running".to_string();
            return;
        }
        let Some(root) = self.index_root.clone() else {
            self.message = "reindex unavailable: run `findex ingest <root>` first".to_string();
            return;
        };
        let db_path = self.db_path.clone();
        let (sender, receiver) = mpsc::channel();
        let spawn = std::thread::Builder::new()
            .name("findex-tui-ingest".to_string())
            .spawn(move || {
                let result = Storage::open(&db_path)
                    .map_err(|error| error.to_string())
                    .and_then(|storage| {
                        ingest_codebase(&root, &db_path, &storage)
                            .map_err(|error| error.to_string())
                    });
                let _ = sender.send(result);
            });
        match spawn {
            Ok(_) => {
                self.indexing = Some(IndexingJob {
                    receiver,
                    started: Instant::now(),
                });
                self.message = "reindexing local workspace".to_string();
            }
            Err(error) => self.message = format!("could not start reindex: {error}"),
        }
    }

    fn poll_indexing(&mut self, storage: &Storage) -> anyhow::Result<()> {
        let result = self
            .indexing
            .as_ref()
            .and_then(|job| job.receiver.try_recv().ok());
        let Some(result) = result else {
            return Ok(());
        };
        self.indexing = None;
        match result {
            Ok(stats) => {
                self.refresh_index(storage)?;
                self.message = format!(
                    "reindex complete: {} changed, {} deleted, {} ms",
                    stats.parsed_files, stats.deleted_files, stats.duration_ms
                );
                self.notify(self.message.clone(), ToastType::Success);
            }
            Err(error) => {
                self.message = format!("reindex failed: {error}");
                self.notify(self.message.clone(), ToastType::Error);
            }
        }
        Ok(())
    }

    fn poll_update(&mut self) {
        let result = self
            .update_job
            .as_ref()
            .and_then(|job| job.receiver.try_recv().ok());
        let Some(result) = result else {
            return;
        };
        self.update_job = None;
        match result {
            Ok(UpdateJobResult::Checked(check)) => {
                self.available_update = check.available;
                if let Some(update) = &self.available_update {
                    self.message =
                        format!("signed update {} available · F8 to review", update.version);
                    self.notify(self.message.clone(), ToastType::Info);
                }
            }
            Ok(UpdateJobResult::Installed(version)) => {
                self.available_update = None;
                self.update_prompt = false;
                self.message = format!("Findex {version} installed · restart to activate");
                self.notify(self.message.clone(), ToastType::Success);
            }
            Err(error) => {
                self.update_prompt = false;
                self.message = format!("update failed: {error}");
                self.notify(self.message.clone(), ToastType::Error);
            }
        }
    }

    fn start_update_install(&mut self) {
        let Some(update) = self.available_update.clone() else {
            self.update_prompt = false;
            return;
        };
        if self.update_job.is_some() {
            return;
        }
        let version = update.version.clone();
        let (sender, receiver) = mpsc::channel();
        let spawn = std::thread::Builder::new()
            .name("findex-update-install".to_string())
            .spawn(move || {
                let result = findex_core::updater::install_update(&update)
                    .map(|_| UpdateJobResult::Installed(version))
                    .map_err(|error| error.to_string());
                let _ = sender.send(result);
            });
        match spawn {
            Ok(_) => {
                self.update_job = Some(UpdateJob { receiver });
                self.update_prompt = false;
                self.message = "downloading and verifying signed update".to_string();
                self.notify(self.message.clone(), ToastType::Info);
            }
            Err(error) => {
                self.update_prompt = false;
                self.message = format!("could not start update: {error}");
                self.notify(self.message.clone(), ToastType::Error);
            }
        }
    }

    fn refresh_index(&mut self, storage: &Storage) -> anyhow::Result<()> {
        self.symbols = storage.list_symbols()?;
        self.files = storage.list_files()?.len();
        self.edges = storage.list_edges()?.len();
        self.graph = graph_snapshot(storage, 220)?;
        Ok(())
    }

    fn run_search(&mut self, storage: &Storage) -> anyhow::Result<()> {
        self.search_dirty_at = None;
        let search_input = self.search_input.lines().join("\n");
        if search_input.trim().is_empty() {
            self.search_results.clear();
            return Ok(());
        }
        let started = Instant::now();
        self.search_results = search_codebase_with_components(
            &self.db_path,
            storage,
            &search_input,
            self.search_mode,
            Some(self.reranker.as_ref()),
            self.embedder.as_ref(),
            50,
        )?;
        self.search_state.select(Some(0));
        self.refresh_source_preview();
        self.message = format!(
            "{} matches · {} ms · {}",
            self.search_results.len(),
            started.elapsed().as_millis(),
            self.search_mode
        );
        Ok(())
    }

    fn move_search(&mut self, delta: i32) {
        if self.search_results.is_empty() {
            return;
        }
        let current = self.search_state.selected().unwrap_or(0) as i32;
        let next = (current + delta).clamp(0, self.search_results.len() as i32 - 1);
        self.search_state.select(Some(next as usize));
        self.refresh_source_preview();
    }

    fn refresh_source_preview(&mut self) {
        self.source_preview = self
            .search_state
            .selected()
            .and_then(|index| self.search_results.get(index))
            .map(|(symbol, _)| highlighted_source(symbol, self.index_root.as_deref()))
            .unwrap_or_default();
    }

    fn inspect_selected(&mut self, storage: &Storage) -> anyhow::Result<()> {
        let Some((symbol, _)) = self
            .search_state
            .selected()
            .and_then(|index| self.search_results.get(index))
        else {
            return Ok(());
        };
        let report = impact_analysis(storage, &symbol.id)?;
        self.inspector = format!(
            "{} {}\n\n{}\n{}:{}-{}\n\nRISK  {:.1}/100{}\nIN    {}\nOUT   {}\nFILES {}\n\nCALLERS\n{}\n\nCALLEES\n{}",
            icon(self.nerd_icons, "󰅩", "@"),
            report.symbol.kind,
            report.symbol.signature,
            report.symbol.file_path,
            report.symbol.start_line,
            report.symbol.end_line,
            report.risk_score,
            if report.god_node { "  GOD NODE" } else { "" },
            report.incoming_edges,
            report.outgoing_edges,
            report.affected_files.len(),
            report
                .callers
                .iter()
                .map(|symbol| format!("  {} · {}", symbol.name, short_path(&symbol.file_path)))
                .collect::<Vec<_>>()
                .join("\n"),
            report
                .callees
                .iter()
                .map(|symbol| format!("  {} · {}", symbol.name, short_path(&symbol.file_path)))
                .collect::<Vec<_>>()
                .join("\n")
        );
        self.impact_tree = TreeState::default();
        self.impact_tree.open(vec!["callers".to_string()]);
        self.impact_tree.open(vec!["callees".to_string()]);
        self.impact_report = Some(report.clone());
        self.change_view(View::Inspector);
        self.message = format!("inspecting {}", report.symbol.name);
        Ok(())
    }

    fn draw(&mut self, frame: &mut Frame) {
        frame.render_widget(
            Block::default().style(Style::default().bg(nord::BG)),
            frame.area(),
        );
        let rows = Layout::vertical([
            Constraint::Length(4),
            Constraint::Min(8),
            Constraint::Length(1),
        ])
        .split(frame.area());
        self.draw_header(frame, rows[0]);
        match self.view {
            View::Dashboard => self.draw_dashboard(frame, rows[1]),
            View::Search => self.draw_search(frame, rows[1]),
            View::Graph => self.draw_graph(frame, rows[1]),
            View::Query => self.draw_query(frame, rows[1]),
            View::Inspector => self.draw_inspector(frame, rows[1]),
            View::Runtime => self.draw_runtime(frame, rows[1]),
        }
        self.draw_footer(frame, rows[2]);
        if self.indexing.is_some() {
            self.draw_indexing(frame);
        }
        if self.help.is_open() {
            self.draw_help(frame);
        }
        if self.update_prompt {
            self.draw_update_prompt(frame);
        }
        if self.motion {
            let area = frame.area();
            self.effects.process_effects(
                Duration::from_millis(80).into(),
                frame.buffer_mut(),
                area,
            );
        }
        self.toasts.set_area(frame.area());
        frame.render_widget(&self.toasts, frame.area());
    }

    fn draw_header(&self, frame: &mut Frame, area: Rect) {
        let pulse = if self.motion {
            ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"][(self.tick as usize / 2) % 10]
        } else {
            "•"
        };
        let rows = Layout::vertical([Constraint::Length(1), Constraint::Length(3)]).split(area);
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(
                    format!(" {} FINDEX ", icon(self.nerd_icons, "󰒋", "F")),
                    Style::default()
                        .fg(nord::BRIGHT)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled("LOCAL CODE GRAPH", Style::default().fg(nord::CYAN)),
                Span::styled(format!(" {pulse} "), Style::default().fg(nord::GREEN)),
                Span::styled(
                    format!(
                        "{} files  {} symbols  {} edges",
                        self.files,
                        self.symbols.len(),
                        self.edges
                    ),
                    Style::default().fg(nord::TEXT),
                ),
            ])),
            rows[0],
        );
        let labels = [
            "1 dashboard",
            "2 search",
            "3 graph",
            "4 query",
            "5 inspect",
            "6 runtime",
        ];
        let tabs = TabNav::new(&labels, self.view as usize)
            .style(Style::default().fg(nord::TEXT))
            .highlight_style(Style::default().fg(nord::CYAN).add_modifier(Modifier::BOLD))
            .border_style(Style::default().fg(nord::BORDER))
            .indicator(Some(icon(self.nerd_icons, "󰅂", ">")));
        frame.render_widget(tabs, rows[1]);
    }

    fn draw_dashboard(&self, frame: &mut Frame, area: Rect) {
        if self.symbols.is_empty() && area.width >= 32 && area.height >= 10 {
            if let Some(logo) = &self.logo {
                frame.render_widget(TerminalImage::new(logo), area);
                return;
            }
            frame.render_widget(
                BigText::builder()
                    .pixel_size(PixelSize::HalfHeight)
                    .style(Style::default().fg(nord::CYAN))
                    .centered()
                    .lines(vec!["FINDEX".into()])
                    .build(),
                area,
            );
            return;
        }
        let columns = Layout::horizontal([Constraint::Percentage(50), Constraint::Percentage(50)])
            .split(area);
        let left = Layout::vertical([Constraint::Percentage(55), Constraint::Percentage(45)])
            .split(columns[0]);
        let right = Layout::vertical([Constraint::Percentage(55), Constraint::Percentage(45)])
            .split(columns[1]);
        let graph_health = if self.symbols.is_empty() {
            0.0
        } else {
            (self.edges as f64 / self.symbols.len() as f64 / 4.0).min(1.0)
        };
        frame.render_widget(
            Gauge::default()
                .block(panel(" index topology "))
                .gauge_style(Style::default().fg(nord::CYAN).bg(nord::PANEL_ALT))
                .ratio(graph_health)
                .label(format!("{:.0}% connected", graph_health * 100.0)),
            left[0],
        );
        let categories = self
            .graph
            .nodes
            .iter()
            .fold([0usize; 4], |mut counts, node| {
                counts[match node.category.as_str() {
                    "god" => 0,
                    "ui" => 1,
                    "api" => 2,
                    _ => 3,
                }] += 1;
                counts
            });
        frame.render_widget(
            Paragraph::new(vec![
                metric_line("God nodes", categories[0], nord::RED),
                metric_line("UI nodes", categories[1], nord::BLUE),
                metric_line("API nodes", categories[2], nord::GREEN),
                metric_line("Code nodes", categories[3], nord::PURPLE),
            ])
            .block(panel(" graph classes ")),
            left[1],
        );
        frame.render_widget(
            Sparkline::default()
                .block(panel(" process memory · MiB "))
                .data(self.memory_history.iter().copied().collect::<Vec<_>>())
                .style(Style::default().fg(nord::GREEN)),
            right[0],
        );
        let merkle = format!(
            "{}  Merkle subtree diff\n{}  Stack Graph exact where supported\n{}  Oxc + tree-sitter AST\n{}  Tantivy + USearch hybrid",
            icon(self.nerd_icons, "󰄬", "ok"),
            icon(self.nerd_icons, "󰄬", "ok"),
            icon(self.nerd_icons, "󰄬", "ok"),
            icon(self.nerd_icons, "󰄬", "ok")
        );
        frame.render_widget(
            Paragraph::new(merkle)
                .style(Style::default().fg(nord::TEXT))
                .block(panel(" retrieval capabilities ")),
            right[1],
        );
    }

    fn draw_indexing(&self, frame: &mut Frame) {
        let area = centered_rect(44, 56, frame.area());
        frame.render_widget(Clear, area);
        frame.render_widget(
            panel(" updating index ").border_style(Style::default().fg(nord::CYAN)),
            area,
        );
        if area.width < crate::ingest_sprite::WIDTH + 4
            || area.height < crate::ingest_sprite::HEIGHT + 4
        {
            return;
        }
        let sprite_x = area.x + (area.width - crate::ingest_sprite::WIDTH) / 2;
        let sprite_y = area.y + 2;
        let frame_index = if self.motion {
            (self.tick as usize / 2) % crate::ingest_sprite::FRAME_COUNT
        } else {
            0
        };
        crate::ingest_sprite::draw(frame.buffer_mut(), sprite_x, sprite_y, frame_index);

        let elapsed = self
            .indexing
            .as_ref()
            .map_or(Duration::ZERO, |job| job.started.elapsed());
        let stages = [
            "scanning changed subtrees",
            "parsing changed files",
            "resolving relationships",
            "updating retrieval indexes",
        ];
        let stage = stages[(elapsed.as_millis() as usize / 900) % stages.len()];
        let text_area = Rect::new(
            area.x + 2,
            sprite_y + crate::ingest_sprite::HEIGHT,
            area.width.saturating_sub(4),
            2,
        );
        frame.render_widget(
            Paragraph::new(format!("{stage}\n{:.1}s elapsed", elapsed.as_secs_f32()))
                .centered()
                .style(Style::default().fg(nord::TEXT)),
            text_area,
        );
    }

    fn draw_search(&mut self, frame: &mut Frame, area: Rect) {
        let rows = Layout::vertical([Constraint::Length(3), Constraint::Min(5)]).split(area);
        let state = if self.search_dirty_at.is_some() {
            "debouncing"
        } else {
            self.search_mode
        };
        self.search_input
            .set_block(panel(format!(" search · {state} · F2 mode ")));
        self.search_input
            .set_style(Style::default().fg(nord::BRIGHT));
        self.search_input
            .set_cursor_style(Style::default().fg(nord::BG).bg(nord::CYAN));
        frame.render_widget(&self.search_input, rows[0]);
        let columns = Layout::horizontal([Constraint::Percentage(62), Constraint::Percentage(38)])
            .split(rows[1]);
        let items: Vec<_> = self
            .search_results
            .iter()
            .map(|(symbol, score)| {
                ListItem::new(Line::from(vec![
                    Span::styled(format!("{:>5.2} ", score), Style::default().fg(nord::CYAN)),
                    Span::styled(
                        format!("{:<10}", symbol.kind.to_ascii_lowercase()),
                        Style::default().fg(kind_color(&symbol.kind)),
                    ),
                    Span::styled(symbol.name.clone(), Style::default().fg(nord::BRIGHT)),
                    Span::styled(
                        format!("  {}:{}", short_path(&symbol.file_path), symbol.start_line),
                        Style::default().fg(nord::BORDER),
                    ),
                ]))
            })
            .collect();
        frame.render_stateful_widget(
            List::new(items)
                .block(panel(format!(" results · {} ", self.search_results.len())))
                .highlight_style(Style::default().fg(nord::BG).bg(nord::BLUE))
                .highlight_symbol("▌"),
            columns[0],
            &mut self.search_state,
        );
        let preview = if self.source_preview.is_empty() {
            vec![Line::from(
                "Type a behavioral query. Search runs after 180 ms of idle time.",
            )]
        } else {
            self.source_preview.clone()
        };
        frame.render_widget(
            Paragraph::new(preview)
                .wrap(Wrap { trim: false })
                .style(Style::default().fg(nord::TEXT))
                .block(panel(" source preview ")),
            columns[1],
        );
    }

    fn draw_graph(&self, frame: &mut Frame, area: Rect) {
        let node_count = self.graph.nodes.len().max(1);
        let positions: HashMap<_, _> = self
            .graph
            .nodes
            .iter()
            .enumerate()
            .map(|(index, node)| {
                let ring = 0.25 + 0.68 * ((index % 4 + 1) as f64 / 4.0);
                let angle = index as f64 / node_count as f64 * TAU * 7.0;
                (node.id.as_str(), (ring * angle.cos(), ring * angle.sin()))
            })
            .collect();
        let graph = &self.graph;
        let canvas = Canvas::default()
            .block(panel(format!(
                " 3D-ready topology · {} nodes · {} links{} ",
                graph.nodes.len(),
                graph.links.len(),
                if graph.truncated { " · bounded" } else { "" }
            )))
            .marker(Marker::Braille)
            .x_bounds([-1.1, 1.1])
            .y_bounds([-1.1, 1.1])
            .paint(|context| {
                for link in graph.links.iter().take(800) {
                    if let (Some(source), Some(target)) = (
                        positions.get(link.source.as_str()),
                        positions.get(link.target.as_str()),
                    ) {
                        context.draw(&CanvasLine {
                            x1: source.0,
                            y1: source.1,
                            x2: target.0,
                            y2: target.1,
                            color: nord::BORDER,
                        });
                    }
                }
                for node in &graph.nodes {
                    if let Some((x, y)) = positions.get(node.id.as_str()) {
                        let marker = match node.category.as_str() {
                            "god" => "●",
                            "ui" => "◆",
                            "api" => "■",
                            _ => "·",
                        };
                        context.print(
                            *x,
                            *y,
                            Span::styled(
                                marker,
                                Style::default().fg(category_color(&node.category)),
                            ),
                        );
                        if node.degree >= 8 {
                            context.print(
                                *x + 0.018,
                                *y,
                                Span::styled(node.name.clone(), Style::default().fg(nord::TEXT)),
                            );
                        }
                    }
                }
            });
        frame.render_widget(canvas, area);
    }

    fn draw_query(&mut self, frame: &mut Frame, area: Rect) {
        let rows = Layout::vertical([Constraint::Length(4), Constraint::Min(4)]).split(area);
        self.query_input
            .set_block(panel(" manual graph query · Enter run "));
        self.query_input
            .set_style(Style::default().fg(nord::BRIGHT));
        self.query_input
            .set_cursor_style(Style::default().fg(nord::BG).bg(nord::CYAN));
        frame.render_widget(&self.query_input, rows[0]);
        let result_block = panel(" result · PageUp/PageDown scroll ");
        let result_area = result_block.inner(rows[1]);
        frame.render_widget(result_block, rows[1]);
        let result_lines = self.query_result.lines().count().max(1) as u16;
        let content_size = Size::new(result_area.width.max(1), result_lines);
        let mut scroll = ScrollView::new(content_size);
        scroll.render_widget(
            Paragraph::new(&*self.query_result)
                .style(Style::default().fg(nord::TEXT))
                .wrap(Wrap { trim: false }),
            Rect::new(0, 0, content_size.width, content_size.height),
        );
        frame.render_stateful_widget(scroll, result_area, &mut self.query_scroll);
    }

    fn draw_inspector(&mut self, frame: &mut Frame, area: Rect) {
        let columns = Layout::horizontal([Constraint::Percentage(58), Constraint::Percentage(42)])
            .split(area);
        frame.render_widget(
            Paragraph::new(&*self.inspector)
                .style(Style::default().fg(nord::TEXT))
                .wrap(Wrap { trim: false })
                .block(panel(" impact inspector ")),
            columns[0],
        );
        let Some(report) = &self.impact_report else {
            frame.render_widget(
                Paragraph::new("Inspect a search result to build its relationship tree.")
                    .style(Style::default().fg(nord::BORDER))
                    .block(panel(" relationship tree ")),
                columns[1],
            );
            return;
        };
        let callers = report
            .callers
            .iter()
            .map(|symbol| {
                TreeItem::new_leaf(
                    symbol.id.clone(),
                    format!(
                        "{}  {}:{}",
                        symbol.name,
                        short_path(&symbol.file_path),
                        symbol.start_line
                    ),
                )
            })
            .collect();
        let callees = report
            .callees
            .iter()
            .map(|symbol| {
                TreeItem::new_leaf(
                    symbol.id.clone(),
                    format!(
                        "{}  {}:{}",
                        symbol.name,
                        short_path(&symbol.file_path),
                        symbol.start_line
                    ),
                )
            })
            .collect();
        let mut items = Vec::with_capacity(2);
        if let Ok(item) = TreeItem::new("callers".to_string(), "Callers", callers) {
            items.push(item);
        }
        if let Ok(item) = TreeItem::new("callees".to_string(), "Callees", callees) {
            items.push(item);
        }
        if let Ok(tree) = Tree::new(&items) {
            frame.render_stateful_widget(
                tree.block(panel(" relationship tree "))
                    .style(Style::default().fg(nord::TEXT))
                    .highlight_style(Style::default().fg(nord::BG).bg(nord::BLUE))
                    .highlight_symbol("▌ ")
                    .node_closed_symbol("+ ")
                    .node_open_symbol("- "),
                columns[1],
                &mut self.impact_tree,
            );
        }
    }

    fn draw_runtime(&self, frame: &mut Frame, area: Rect) {
        let columns = Layout::horizontal([Constraint::Percentage(55), Constraint::Percentage(45)])
            .split(area);
        let left = Layout::vertical([
            Constraint::Length(3),
            Constraint::Length(3),
            Constraint::Min(4),
        ])
        .split(columns[0]);
        let ram_ratio = if self.runtime.total_memory_bytes == 0 {
            0.0
        } else {
            1.0 - self.runtime.available_memory_bytes as f64
                / self.runtime.total_memory_bytes as f64
        };
        frame.render_widget(
            Gauge::default()
                .block(panel(" system RAM "))
                .ratio(ram_ratio.clamp(0.0, 1.0))
                .label(format!("{:.1}% used", ram_ratio * 100.0))
                .gauge_style(Style::default().fg(nord::BLUE).bg(nord::PANEL_ALT)),
            left[0],
        );
        let budget_ratio = if self.runtime.memory_budget_bytes == 0 {
            0.0
        } else {
            self.runtime.process_memory_bytes as f64 / self.runtime.memory_budget_bytes as f64
        };
        frame.render_widget(
            Gauge::default()
                .block(panel(" Findex memory budget "))
                .ratio(budget_ratio.clamp(0.0, 1.0))
                .label(format!(
                    "{:.0} / {:.0} MiB",
                    self.runtime.process_memory_bytes as f64 / 1_048_576.0,
                    self.runtime.memory_budget_bytes as f64 / 1_048_576.0
                ))
                .gauge_style(
                    Style::default()
                        .fg(if budget_ratio > 0.85 {
                            nord::RED
                        } else {
                            nord::GREEN
                        })
                        .bg(nord::PANEL_ALT),
                ),
            left[1],
        );
        frame.render_widget(
            Paragraph::new(format!(
                "logical CPU      {}\nRayon workers    {}\nembedding batch  {}\nquantization     {}\nCUDA compiled    {}",
                self.runtime.logical_cpus,
                self.runtime.rayon_threads,
                self.runtime.recommended_embedding_batch,
                self.runtime.vector_quantization,
                self.runtime.cuda_compiled
            ))
            .style(Style::default().fg(nord::TEXT))
            .block(panel(" compute policy ")),
            left[2],
        );
        let gpu_text = if self.runtime.gpu_devices.is_empty() {
            "No NVIDIA telemetry available.\n\nCPU execution remains enabled; compile with --features cuda and install a compatible ONNX Runtime/CUDA stack to opt in."
                .to_string()
        } else {
            self.runtime
                .gpu_devices
                .iter()
                .map(|gpu| {
                    format!(
                        "{}\n{} / {} MiB\nutilization {}%\ntemperature {}",
                        gpu.name,
                        gpu.used_memory_mib,
                        gpu.total_memory_mib,
                        gpu.utilization_percent,
                        gpu.temperature_celsius
                            .map_or("n/a".to_string(), |value| format!("{value}°C"))
                    )
                })
                .collect::<Vec<_>>()
                .join("\n\n")
        };
        let right = Layout::vertical([Constraint::Percentage(62), Constraint::Percentage(38)])
            .split(columns[1]);
        frame.render_widget(
            Paragraph::new(gpu_text)
                .style(Style::default().fg(nord::TEXT))
                .wrap(Wrap { trim: false })
                .block(panel(" GPU telemetry · r refresh ")),
            right[0],
        );
        frame.render_widget(
            TuiLoggerWidget::default()
                .block(panel(" diagnostics "))
                .style(Style::default().fg(nord::TEXT))
                .style_info(Style::default().fg(nord::CYAN))
                .style_warn(Style::default().fg(nord::YELLOW))
                .style_error(Style::default().fg(nord::RED))
                .output_timestamp(Some("%H:%M:%S".to_string()))
                .output_level(Some(TuiLoggerLevelOutput::Abbreviated))
                .output_target(false)
                .output_file(false)
                .output_line(false),
            right[1],
        );
    }

    fn draw_footer(&self, frame: &mut Frame, area: Rect) {
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(
                    format!(" {} ", self.message),
                    Style::default().fg(nord::GREEN),
                ),
                Span::styled(
                    if self.available_update.is_some() {
                        " F8 update  Tab views  r refresh  ? help  Ctrl+Q quit "
                    } else {
                        " Tab views  r reindex/refresh  Enter inspect/run  ? help  Ctrl+Q quit "
                    },
                    Style::default().fg(nord::BORDER),
                ),
            ])),
            area,
        );
    }

    fn draw_help(&mut self, frame: &mut Frame) {
        let overlay = Overlay::new()
            .width(Constraint::Percentage(62))
            .height(Constraint::Percentage(62))
            .slide(Slide::Bottom)
            .backdrop(Backdrop::new(nord::BG).fg(nord::BORDER))
            .bg(nord::PANEL)
            .block(
                panel(" keyboard + resource controls ")
                    .border_style(Style::default().fg(nord::CYAN)),
            );
        frame.render_stateful_widget(overlay, frame.area(), &mut self.help);
        if let Some(area) = self.help.inner_area() {
            frame.render_widget(
            Paragraph::new(
                "1–6 / Tab    switch views\n/             jump to search\nF2            hybrid → lexical → semantic\nF8            review a signed update\n↑ ↓           select search result\nEnter         inspect result or execute query\nr             reindex (dashboard) or refresh GPU probes (runtime)\n?             close this help\nCtrl+Q / Esc  quit\n\nEnvironment\nFINDEX_TUI_MOTION=0      disable motion\nFINDEX_TUI_ICONS=ascii   glyph fallback\nFINDEX_MEMORY_BUDGET_MB  hard policy target\nFINDEX_RAYON_THREADS     worker count",
            )
            .style(Style::default().fg(nord::TEXT))
            .wrap(Wrap { trim: false }),
            area,
            );
        }
    }

    fn draw_update_prompt(&self, frame: &mut Frame) {
        let Some(update) = &self.available_update else {
            return;
        };
        let area = centered_rect(54, 42, frame.area());
        frame.render_widget(Clear, area);
        frame.render_widget(
            Paragraph::new(format!(
                "Findex {} is available for {}.\n\n{}\n\nThe archive will be downloaded over HTTPS and verified with the release Minisign key before the executable is replaced.\n\nEnter  install      Esc  keep current version",
                update.version,
                update.target,
                update.notes.trim()
            ))
            .style(Style::default().fg(nord::TEXT))
            .wrap(Wrap { trim: false })
            .block(
                panel(" signed update · confirmation required ")
                    .border_style(Style::default().fg(nord::GREEN)),
            ),
            area,
        );
    }
}

fn start_update_check() -> Option<UpdateJob> {
    if !findex_core::updater::updater_enabled() {
        return None;
    }
    let (sender, receiver) = mpsc::channel();
    std::thread::Builder::new()
        .name("findex-update-check".to_string())
        .spawn(move || {
            let result = findex_core::updater::check_for_update(false)
                .map(UpdateJobResult::Checked)
                .map_err(|error| error.to_string());
            let _ = sender.send(result);
        })
        .ok()?;
    Some(UpdateJob { receiver })
}

fn panel(title: impl Into<Line<'static>>) -> Block<'static> {
    Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(nord::token("border.unfocused")))
        .style(Style::default().bg(nord::PANEL))
        .title(title.into().style(Style::default().fg(nord::CYAN)))
}

fn metric_line(label: &str, value: usize, color: Color) -> Line<'static> {
    Line::from(vec![
        Span::styled(
            format!("{value:>6}"),
            Style::default().fg(color).add_modifier(Modifier::BOLD),
        ),
        Span::styled(format!("  {label}"), Style::default().fg(nord::TEXT)),
    ])
}

fn icon<'a>(nerd: bool, nerd_icon: &'a str, fallback: &'a str) -> &'a str {
    if nerd {
        nerd_icon
    } else {
        fallback
    }
}

fn kind_color(kind: &str) -> Color {
    let kind = kind.to_ascii_lowercase();
    if kind.contains("component") || kind.contains("widget") {
        nord::BLUE
    } else if kind.contains("function") || kind.contains("method") {
        nord::GREEN
    } else if kind.contains("class") || kind.contains("struct") || kind.contains("interface") {
        nord::PURPLE
    } else {
        nord::YELLOW
    }
}

fn category_color(category: &str) -> Color {
    match category {
        "god" => nord::RED,
        "ui" => nord::BLUE,
        "api" => nord::GREEN,
        _ => nord::PURPLE,
    }
}

fn highlighted_source(symbol: &Symbol, root: Option<&std::path::Path>) -> Vec<Line<'static>> {
    let mut path = PathBuf::from(&symbol.file_path);
    if path.is_relative() {
        if let Some(root) = root {
            path = root.join(path);
        }
    }
    let Ok(source) = std::fs::read_to_string(&path) else {
        return vec![
            Line::styled(symbol.signature.clone(), Style::default().fg(nord::BRIGHT)),
            Line::styled(
                format!(
                    "{}:{}-{}",
                    symbol.file_path, symbol.start_line, symbol.end_line
                ),
                Style::default().fg(nord::BORDER),
            ),
        ];
    };
    static SYNTAXES: std::sync::OnceLock<SyntaxSet> = std::sync::OnceLock::new();
    static THEMES: std::sync::OnceLock<ThemeSet> = std::sync::OnceLock::new();
    let syntaxes = SYNTAXES.get_or_init(SyntaxSet::load_defaults_newlines);
    let themes = THEMES.get_or_init(ThemeSet::load_defaults);
    let extension = path
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or("txt");
    let syntax = syntaxes
        .find_syntax_by_extension(extension)
        .unwrap_or_else(|| syntaxes.find_syntax_plain_text());
    let theme = themes
        .themes
        .get("base16-ocean.dark")
        .or_else(|| themes.themes.values().next())
        .expect("syntect ships at least one theme");
    let start = symbol.start_line.saturating_sub(3).max(1);
    let end = symbol
        .end_line
        .saturating_add(3)
        .min(start.saturating_add(48));
    let excerpt = source
        .lines()
        .enumerate()
        .filter(|(index, _)| {
            let line = index + 1;
            line >= start && line <= end
        })
        .map(|(_, line)| format!("{line}\n"))
        .collect::<String>();
    let mut highlighter = HighlightLines::new(syntax, theme);
    let mut rendered = Vec::new();
    for (offset, line) in LinesWithEndings::from(&excerpt).enumerate() {
        let ranges = highlighter
            .highlight_line(line, syntaxes)
            .unwrap_or_default();
        let mut spans = vec![Span::styled(
            format!("{:>5} ", start + offset),
            Style::default().fg(nord::BORDER),
        )];
        spans.extend(ranges.into_iter().map(|(style, text)| {
            let mut terminal_style = Style::default().fg(Color::Rgb(
                style.foreground.r,
                style.foreground.g,
                style.foreground.b,
            ));
            if style.font_style.contains(FontStyle::BOLD) {
                terminal_style = terminal_style.add_modifier(Modifier::BOLD);
            }
            if style.font_style.contains(FontStyle::ITALIC) {
                terminal_style = terminal_style.add_modifier(Modifier::ITALIC);
            }
            Span::styled(
                text.trim_end_matches(['\r', '\n']).to_string(),
                terminal_style,
            )
        }));
        rendered.push(Line::from(spans));
    }
    rendered
}

fn load_terminal_logo() -> Option<Protocol> {
    let path = std::env::var_os("FINDEX_TUI_IMAGE").map(PathBuf::from)?;
    let image = image::ImageReader::open(path).ok()?.decode().ok()?;
    let picker = Picker::from_query_stdio().unwrap_or_else(|_| Picker::halfblocks());
    let font = picker.font_size();
    let natural = Size::new(
        image.width().div_ceil(font.width as u32).min(72) as u16,
        image.height().div_ceil(font.height as u32).min(24) as u16,
    );
    picker
        .new_protocol(image, natural, ImageResize::Fit(None))
        .ok()
}

fn short_path(path: &str) -> String {
    let parts: Vec<_> = path.rsplit(['/', '\\']).take(2).collect();
    parts.into_iter().rev().collect::<Vec<_>>().join("/")
}

fn centered_rect(percent_x: u16, percent_y: u16, area: Rect) -> Rect {
    let vertical = Layout::vertical([
        Constraint::Percentage((100 - percent_y) / 2),
        Constraint::Percentage(percent_y),
        Constraint::Percentage((100 - percent_y) / 2),
    ])
    .split(area);
    Layout::horizontal([
        Constraint::Percentage((100 - percent_x) / 2),
        Constraint::Percentage(percent_x),
        Constraint::Percentage((100 - percent_x) / 2),
    ])
    .split(vertical[1])[1]
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;

    #[test]
    fn every_view_renders_with_ascii_fallback() {
        let directory = tempfile::tempdir().unwrap();
        let mut app = App::new(directory.path().join("db")).unwrap();
        app.nerd_icons = false;
        app.motion = false;
        let mut terminal = Terminal::new(TestBackend::new(128, 42)).unwrap();

        for view in View::ALL {
            app.view = view;
            terminal.draw(|frame| app.draw(frame)).unwrap();
        }

        let rendered = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(rendered.contains("FINDEX"));
    }
}
