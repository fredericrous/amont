//! The dashboard.
//!
//! Rendering is a pure function of state, which is the only reason the spec's
//! success criterion can be a test rather than an intention: `TestBackend`
//! draws into a buffer and the assertion reads it back. A criterion nobody can
//! run is a wish.
//!
//! Layout follows Shneiderman's mantra — a fleet summary, then a filterable
//! table, then detail on demand. Every count carries its denominator, because
//! the two failures that motivated this tool were both a bare scalar printed
//! with equal confidence whether it had measured everything or nothing.
//!
//! State is encoded twice, glyph AND colour, and never by colour alone: roughly
//! 8% of men have red-green colour vision deficiency, and `NO_COLOR` must leave
//! a fully usable screen.

use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, Cell, Paragraph, Row, Table, TableState};

use crate::checks;
use crate::scan::{AgentsMdState, FleetScan, Repo};
use crate::shim::{BakeState, ShimState, DISPATCHERS};

/// What the screen is showing, and what a keystroke therefore means.
///
/// Previously this was four independent booleans — `detail`, `hook_view`,
/// `filtering`, plus `scanning` — which describes sixteen states of which four
/// are meaningful. Adding a command palette and a type-to-confirm prompt would
/// have taken it to sixty-four. Making the invalid combinations unrepresentable
/// is what keeps the next two features testable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Browse,
    Detail,
    HookView,
    /// Typing into the filter. The row list narrows as you type.
    Filter,
}

/// A keystroke, named. The previous signature was
/// `on_key(char, bool, bool, bool)`, where a caller had to remember that the
/// second bool meant Enter, and `'\0'` meant "not a character".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    Char(char),
    Enter,
    Esc,
    Backspace,
    Up,
    Down,
}

/// One line of editable text with a cursor at the end.
///
/// Extracted because the filter, the command palette and the type-the-name
/// confirmation are the same thing wearing different prompts. Building it once
/// is the point of this refactor.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct LineEdit {
    value: String,
}

impl LineEdit {
    pub fn insert(&mut self, c: char) {
        self.value.push(c);
    }
    pub fn backspace(&mut self) {
        self.value.pop();
    }
    pub fn clear(&mut self) {
        self.value.clear();
    }
    pub fn is_empty(&self) -> bool {
        self.value.is_empty()
    }
    pub fn as_str(&self) -> &str {
        &self.value
    }
    /// Test-only until a real caller exists. The palette and the
    /// type-to-confirm prompt will want it; shipping it as public dead code on
    /// that promise is how `Progress::Found` sat unused while pretending the
    /// spec was implemented.
    #[cfg(test)]
    pub fn set(&mut self, v: &str) {
        self.value = v.to_string();
    }
}

pub struct App {
    pub scan: FleetScan,
    pub selected: usize,
    pub mode: Mode,
    /// Where Esc returns to when a prompt is dismissed.
    prev_mode: Mode,
    pub filter: LineEdit,
    /// Which check the detail view has highlighted.
    pub check_selected: usize,
    /// The pending toggle, awaiting a typed confirmation.
    /// What just happened, and how to take it back.
    pub notice: Option<String>,
    pub undo: Option<crate::skips::SkipPlan>,
    pub scanning: bool,
    /// Whether to emit colour. Read from the environment once at construction
    /// rather than consulted per cell: `colors_enabled()` memoises, so a test
    /// could otherwise never exercise both branches in one run.
    pub color: bool,
    pub visited: usize,
    pub elapsed: f64,
    pub quit: bool,
}

impl App {
    pub fn new(scan: FleetScan) -> Self {
        App {
            scan,
            selected: 0,
            mode: Mode::Browse,
            prev_mode: Mode::Browse,
            filter: LineEdit::default(),
            check_selected: 0,
            notice: None,
            undo: None,
            scanning: false,
            color: amont_runtime::ui::colors_enabled(),
            visited: 0,
            elapsed: 0.0,
            quit: false,
        }
    }

    pub fn rows(&self) -> Vec<&Repo> {
        self.scan
            .repos
            .iter()
            .filter(|repo| {
                self.filter.is_empty()
                    || repo
                        .path
                        .to_string_lossy()
                        .to_lowercase()
                        .contains(&self.filter.as_str().to_lowercase())
            })
            .collect()
    }

    pub fn on_key(&mut self, key: Key) {
        match self.mode {
            Mode::Filter => self.filter_key(key),
            Mode::Detail => self.detail_key(key),
            _ => self.browse_key(key),
        }
    }

    fn filter_key(&mut self, key: Key) {
        match key {
            Key::Esc => {
                self.filter.clear();
                self.mode = self.prev_mode;
            }
            // Enter keeps the filter and leaves the prompt.
            Key::Enter => self.mode = self.prev_mode,
            Key::Backspace => self.filter.backspace(),
            Key::Char(c) => self.filter.insert(c),
            Key::Up | Key::Down => {}
        }
        self.selected = 0;
    }

    /// Detail adds a selectable check list and the `s` toggle.
    fn detail_key(&mut self, key: Key) {
        let n = crate::checks::all_checks().len();
        match key {
            Key::Char('j') | Key::Down => {
                self.check_selected = (self.check_selected + 1).min(n - 1)
            }
            Key::Char('k') | Key::Up => self.check_selected = self.check_selected.saturating_sub(1),
            Key::Char('s') => self.begin_toggle(),
            Key::Char('u') => self.take_back(),
            _ => self.browse_key(key),
        }
    }

    fn selected_repo_path(&self) -> Option<std::path::PathBuf> {
        let rows = self.rows();
        let r = rows.get(self.selected)?;
        Some(self.scan.root.join(&r.path))
    }

    fn begin_toggle(&mut self) {
        let Some(repo) = self.selected_repo_path() else {
            return;
        };
        let Some(check) = crate::checks::all_checks()
            .get(self.check_selected)
            .copied()
        else {
            return;
        };
        let plan = crate::skips::plan(&repo, check);
        if let Some(r) = &plan.refuse {
            self.notice = Some(format!("refused: {r}"));
            return;
        }
        // No confirmation step: a check id names exactly one check, so a
        // toggle can no longer suppress anything the user did not point at.
        // The type-the-name friction existed because substring matching could
        // silently take four checks when you asked for one.
        self.commit_toggle(plan);
    }

    /// Write, then record how to reverse it. An undo helps when the user was
    /// wrong; a confirmation only interrupts when they were right.
    fn commit_toggle(&mut self, plan: crate::skips::SkipPlan) {
        let Some(repo) = self.selected_repo_path() else {
            return;
        };
        match crate::skips::apply(&repo, &plan) {
            Ok(()) => {
                let verb = match plan.action {
                    crate::skips::Action::Add => "skipped",
                    crate::skips::Action::Remove => "un-skipped",
                };
                self.notice = Some(format!("{verb} {} · u to undo", plan.check));
                self.undo = Some(crate::skips::plan(&repo, plan.check));
                self.refresh_selected_repo();
            }
            Err(e) => self.notice = Some(format!("failed: {e}")),
        }
    }

    fn take_back(&mut self) {
        let (Some(repo), Some(plan)) = (self.selected_repo_path(), self.undo.take()) else {
            return;
        };
        match crate::skips::apply(&repo, &plan) {
            Ok(()) => {
                self.notice = Some(format!("undone: {}", plan.check));
                self.refresh_selected_repo();
            }
            Err(e) => self.notice = Some(format!("undo failed: {e}")),
        }
    }

    /// Re-read this repo's skips so the screen shows what is now true rather
    /// than what was intended.
    fn refresh_selected_repo(&mut self) {
        let Some(path) = self.selected_repo_path() else {
            return;
        };
        let fresh = crate::skips::read(&path);
        let rows = self.rows();
        let Some(target) = rows.get(self.selected).map(|r| r.path.clone()) else {
            return;
        };
        if let Some(repo) = self.scan.repos.iter_mut().find(|repo| repo.path == target) {
            repo.skips = fresh;
        }
    }
    fn browse_key(&mut self, key: Key) {
        let len = self.rows().len();
        match key {
            Key::Char('q') => self.quit = true,
            Key::Char('/') => {
                self.prev_mode = self.mode;
                self.mode = Mode::Filter;
            }
            Key::Char('h') => {
                self.mode = if self.mode == Mode::HookView {
                    Mode::Browse
                } else {
                    Mode::HookView
                }
            }
            Key::Char('j') | Key::Down if len > 0 => {
                self.selected = (self.selected + 1).min(len - 1)
            }
            Key::Char('k') | Key::Up => self.selected = self.selected.saturating_sub(1),
            Key::Enter if len > 0 => self.mode = Mode::Detail,
            Key::Esc => {
                // Leave the current screen first; only then clear a filter.
                if self.mode != Mode::Browse {
                    self.mode = Mode::Browse;
                } else {
                    self.filter.clear();
                }
            }
            _ => {}
        }
    }
}

/// Semantic colour, in the TERMINAL's palette.
///
/// ratatui's named colours emit base ANSI (30-37 / 90-97), which a terminal
/// theme remaps — unlike the 256-colour cube, which is fixed. So these follow
/// whatever palette is configured, including a `vivid`-generated one, without
/// reading any file. `LS_COLORS` is not consulted: it maps file types, not
/// ok/warning/error.
///
/// Returns `Style::default()` when colour is off, so `NO_COLOR` yields a screen
/// that is still fully legible — the glyph and the word carry the state, the
/// colour only reinforces it.
fn tint(on: bool, c: Color) -> Style {
    if on {
        Style::default().fg(c)
    } else {
        Style::default()
    }
}

/// Structure, as opposed to status.
///
/// Green/yellow/red are spoken for — they mean ok/warning/error — so headers
/// and the cursor need a slot that carries no verdict. Which HUE this is
/// remains the terminal's business; only the slot is chosen here.
const ACCENT: Color = Color::Cyan;

/// Named once so the pane and its tests cannot drift apart.
const DECLARED_FILE: &str = amont_runtime::manifest::MANIFEST;

/// `*` or `*.sh,*.bash`, as the manifest writes it.
fn scope_of(exts: &[String]) -> String {
    if exts.is_empty() {
        "*".to_string()
    } else {
        exts.iter()
            .map(|ext| format!("*{ext}"))
            .collect::<Vec<_>>()
            .join(",")
    }
}

/// How the highlighted row is marked.
///
/// Reverse video, NOT an accent foreground. A row style with `fg` overrides the
/// per-cell colour, so accenting the selected row erased its ok/drifted status —
/// on precisely the row the reader is looking at. Reverse swaps whatever colour
/// the cell already has, so an ok row reads as selected AND green.
///
/// The accent goes on the cursor instead; see `selection_cursor`.
fn selection_style() -> Style {
    Style::default().add_modifier(Modifier::REVERSED)
}

/// The `> ` cursor, in the accent when colour is available.
///
/// Three independent signals mark the selection: the symbol itself, reverse
/// video, and this colour. Under `NO_COLOR` the first two remain.
fn selection_cursor(color: bool) -> Span<'static> {
    Span::styled("> ", tint(color, ACCENT).add_modifier(Modifier::BOLD))
}

/// `●` ok, `◐` drifted, `○` missing — position encodes WHICH hook, so the
/// column costs four characters instead of four names.
/// The repository path as the table shows it.
///
/// Split out to be testable: a `Frame` renderer is not, and this is the one
/// place scanned-disk text becomes terminal output in the dashboard. ratatui's
/// `Cell` keeps zero-width graphemes and crossterm writes them through, so a
/// directory named with control bytes in it would be rendered verbatim.
fn repo_cell(r: &Repo) -> String {
    amont_runtime::ui::sanitize_path(&r.path)
}

/// One glyph per dispatcher.
///
/// `!` for a symlink and `?` for an unreadable file are deliberately not
/// variations on the filled/half/empty circle: those three form a scale from
/// healthy to absent, and neither of the new states is a point on it. A
/// dispatcher that is a link to a tracked file is not "somewhat installed", it
/// is a hazard — writing through it rewrites the other file — and it must not
/// read as a milder ◐.
fn shim_glyphs(r: &Repo) -> String {
    r.shims
        .iter()
        .map(|s| match s {
            ShimState::Ok { .. } => '●',
            ShimState::Drifted => '◐',
            ShimState::Missing => '○',
            ShimState::Symlink { .. } => '!',
            ShimState::Unreadable { .. } => '?',
        })
        .collect()
}

fn bake_word(b: &BakeState) -> &'static str {
    match b {
        BakeState::Current => "current",
        // Not "stale": the repository ships its own binary and the package
        // manager keeps it current. The BAKE column saying "stale" is what
        // would send somebody to run the fix that breaks it.
        BakeState::SelfManaged { .. } => "npm",
        BakeState::Stale { .. } => "stale",
        BakeState::Unbaked => "unbaked",
        BakeState::Mixed => "mixed",
        BakeState::None => "-",
    }
}

/// `-` for missing, not a colour: the pointer is opt-in, so a repo that never
/// added it is not a problem the way a drifted one is.
fn agents_md_word(s: AgentsMdState) -> &'static str {
    match s {
        AgentsMdState::UpToDate => "ok",
        AgentsMdState::Missing => "-",
        AgentsMdState::Drifted => "drift",
        AgentsMdState::Malformed => "bad",
    }
}

/// A redundant text summary of the same information the glyphs carry, so the
/// screen survives NO_COLOR and colour vision deficiency.
fn state_word(r: &Repo) -> String {
    // Before "unmanaged", because these two say WHY it is not managed and
    // "unmanaged" alone would send somebody looking for a missing install.
    if let Some(owner) = &r.shares_hooks_with {
        return format!("covered by {}", amont_runtime::ui::sanitize_path(owner));
    }
    // Named specifically, because "hooks elsewhere" reads like a path quirk when
    // what it actually means is that this repository runs no checks at all.
    if r.hooks_dir.is_hostile_redirect() {
        return match &r.hooks_dir {
            crate::scan::HooksDir::Redirected { path, .. } => {
                match amont_runtime::install::redirect_culprit(path) {
                    Some(tool) => format!("! {tool} owns hooks"),
                    None => "! dispatch taken".into(),
                }
            }
            _ => unreachable!("is_hostile_redirect implies Redirected"),
        };
    }
    if r.hooks_dir.inside().is_none() {
        return "! hooks elsewhere".into();
    }
    if !r.managed {
        return "! unmanaged".into();
    }
    // A symlinked or unreadable dispatcher outranks drift and absence: it is the
    // state in which a write goes somewhere we cannot see.
    let hazard = r
        .shims
        .iter()
        .filter(|s| matches!(s, ShimState::Symlink { .. } | ShimState::Unreadable { .. }))
        .count();
    if hazard > 0 {
        return format!("! not a file {hazard}");
    }
    let missing = r
        .shims
        .iter()
        .filter(|s| matches!(s, ShimState::Missing))
        .count();
    let drifted = r
        .shims
        .iter()
        .filter(|s| matches!(s, ShimState::Drifted))
        .count();
    if drifted > 0 {
        format!("x drifted {drifted}")
    } else if missing > 0 {
        format!("x missing {missing}")
    } else if !r.stale_ours.is_empty() || !r.foreign_subs.is_empty() || r.hook_pkgjson {
        "! leftovers".into()
    } else if matches!(r.baked, BakeState::Stale { .. } | BakeState::Mixed) {
        "! stale bake".into()
    } else {
        "ok".into()
    }
}

pub fn draw(f: &mut Frame, app: &App) {
    let area = f.area();
    let chunks = Layout::vertical([
        Constraint::Length(4),
        Constraint::Min(3),
        Constraint::Length(1),
    ])
    .split(area);

    header(f, chunks[0], app);
    if app.mode == Mode::HookView {
        hooks_view(f, chunks[1], app);
    } else if app.mode == Mode::Detail {
        detail(f, chunks[1], app);
    } else if app.scan.looks_like_a_failed_scan() && !app.scanning {
        failure(f, chunks[1], app);
    } else {
        table(f, chunks[1], app);
    }
    footer(f, chunks[2], app);
}

fn header(f: &mut Frame, area: Rect, app: &App) {
    let s = &app.scan;
    let status = if app.scanning {
        format!(
            "scanning · {} directories · {:.1}s",
            app.visited, app.elapsed
        )
    } else {
        format!("{} directories · {:.1}s", s.dirs_visited, app.elapsed)
    };
    let text = vec![
        // Only the title line is accented; tinting the counts would make the
        // numbers harder to read for decoration's sake.
        Line::from(format!("{}   {status}", s.root.display())).style(tint(app.color, ACCENT)),
        Line::from(format!(
            "{} repositories · {} managed · {} unmanaged · {} skipped subtrees{}",
            s.git_dirs_found,
            s.managed_seen,
            s.unmanaged_seen,
            s.excluded_dirs,
            // Silent at zero: a clean fleet's header says nothing about
            // bypasses, so the line only appears when there is something
            // to act on.
            if s.bypassed_commits == 0 {
                String::new()
            } else {
                format!(
                    " · {} unverified commits in {} repos",
                    s.bypassed_commits, s.repos_with_bypasses
                )
            }
        )),
        Line::from(format!(
            "consistency  {}",
            DISPATCHERS
                .iter()
                .enumerate()
                .map(|(i, n)| {
                    let ok = s
                        .repos
                        .iter()
                        .filter(|r| r.managed)
                        .filter(|r| matches!(r.shims.get(i), Some(ShimState::Ok { .. })))
                        .count();
                    // N/M, never a bare adjective: the number that proves fleet
                    // health is the one the old text sweep got wrong.
                    format!("{n} {ok}/{}", s.managed_seen)
                })
                .collect::<Vec<_>>()
                .join("  ")
        )),
    ];
    f.render_widget(
        Paragraph::new(text).block(Block::default().borders(Borders::BOTTOM)),
        area,
    );
}

/// The screen this tool exists for. An empty table must never read as a calm,
/// clean fleet.
fn failure(f: &mut Frame, area: Rect, app: &App) {
    let s = &app.scan;
    let mut lines = vec![
        Line::from(format!("No repositories found under {}", s.root.display())),
        Line::from(""),
        Line::from(format!(
            "Visited {} directories in {:.1}s and found 0 git repositories.",
            s.dirs_visited, app.elapsed
        )),
        Line::from("This is a SCAN FAILURE, not a clean fleet."),
        Line::from(format!(
            "  - is --root correct?      (currently: {})",
            s.root.display()
        )),
        Line::from(format!(
            "  - is --depth deep enough? (currently: {})",
            s.depth
        )),
    ];
    if !s.unreadable.is_empty() {
        lines.push(Line::from(format!(
            "  - {} path(s) could not be read",
            s.unreadable.len()
        )));
    }
    f.render_widget(Paragraph::new(lines), area);
}

fn table(f: &mut Frame, area: Rect, app: &App) {
    let rows = app.rows();
    // Narrow terminals drop columns rather than scroll sideways.
    let wide = area.width >= 100;
    let mid = area.width >= 76;

    let header = if wide {
        vec![
            "REPO", "SHIMS", "BAKE", "LANG", "SKIPS", "WARN", "DECL", "BYPASS", "AGENTS", "STATE",
        ]
    } else if mid {
        vec!["REPO", "SHIMS", "BAKE", "STATE"]
    } else {
        vec!["REPO", "STATE"]
    };

    let body: Vec<Row> = rows
        .iter()
        .map(|repo| {
            let mut cells = vec![Cell::from(repo_cell(repo))];
            if mid {
                let g = shim_glyphs(repo);
                let all_ok = g.chars().all(|c| c == '●');
                cells.push(Cell::from(g).style(tint(
                    app.color,
                    if all_ok { Color::Green } else { Color::Red },
                )));
                cells.push(Cell::from(bake_word(&repo.baked)));
            }
            if wide {
                cells.push(Cell::from(repo.languages.join(" ")));
                cells.push(Cell::from(if repo.skips.is_empty() {
                    "-".to_string()
                } else {
                    repo.skips.len().to_string()
                }));
                // Downgrades get their own column rather than being folded into
                // SKIPS. They are not the same thing and the difference is the
                // point: a skipped check is silent, a downgraded one prints its
                // failure in red and lets the commit through anyway. Summing
                // them would hide exactly the case worth seeing.
                let weakened = repo.severities.iter().filter(|e| e.weakens()).count();
                cells.push(
                    Cell::from(if weakened == 0 {
                        "-".to_string()
                    } else {
                        weakened.to_string()
                    })
                    .style(tint(
                        app.color,
                        if weakened == 0 {
                            Color::Reset
                        } else {
                            Color::Yellow
                        },
                    )),
                );
                // Declared checks, with unusable lines called out. A count
                // alone would let "3 custom checks" and "3 custom checks, two
                // of which have never run" render identically.
                let broken = repo.declared.iter().filter(|d| d.is_unusable()).count();
                let text = match (repo.declared.len(), broken) {
                    (0, _) => "-".to_string(),
                    (n, 0) => n.to_string(),
                    (n, b) => format!("{n}!{b}"),
                };
                cells.push(Cell::from(text).style(tint(
                    app.color,
                    if broken > 0 { Color::Red } else { Color::Reset },
                )));
                // Bypasses are events, not policy: a downgrade (WARN) is a
                // statement about the repository, a bypass is something that
                // HAPPENED — folding them together would hide which one a
                // rising number means. Recency drives the tint: three dodges
                // this month are a signal, three from two years ago are
                // history.
                let recent = repo.bypasses.last.is_some_and(|last| {
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_secs().saturating_sub(last) < 30 * 86_400)
                        .unwrap_or(false)
                });
                cells.push(
                    Cell::from(if repo.bypasses.total == 0 {
                        "-".to_string()
                    } else {
                        repo.bypasses.total.to_string()
                    })
                    .style(tint(
                        app.color,
                        if recent { Color::Yellow } else { Color::Reset },
                    )),
                );
                cells.push(Cell::from(agents_md_word(repo.agents_md)).style(tint(
                    app.color,
                    match repo.agents_md {
                        AgentsMdState::UpToDate => Color::Green,
                        AgentsMdState::Missing => Color::Reset,
                        AgentsMdState::Drifted | AgentsMdState::Malformed => Color::Red,
                    },
                )));
            }
            let word = state_word(repo);
            let colour = if word.starts_with('x') {
                Color::Red
            } else if word.starts_with('!') {
                Color::Yellow
            } else {
                Color::Green
            };
            cells.push(Cell::from(word).style(tint(app.color, colour)));
            Row::new(cells)
        })
        .collect();

    let widths: Vec<Constraint> = if wide {
        vec![
            Constraint::Min(28),
            Constraint::Length(6),
            Constraint::Length(8),
            Constraint::Length(12),
            Constraint::Length(6),
            Constraint::Length(5),
            Constraint::Length(6),
            Constraint::Length(6),
            Constraint::Length(7),
            Constraint::Length(14),
        ]
    } else if mid {
        vec![
            Constraint::Min(20),
            Constraint::Length(6),
            Constraint::Length(8),
            Constraint::Length(14),
        ]
    } else {
        vec![Constraint::Min(12), Constraint::Length(14)]
    };

    let mut state = TableState::default();
    state.select(Some(app.selected.min(rows.len().saturating_sub(1))));
    f.render_stateful_widget(
        Table::new(body, widths)
            .header(Row::new(header).style(tint(app.color, ACCENT)))
            .row_highlight_style(selection_style())
            .highlight_symbol(selection_cursor(app.color)),
        area,
        &mut state,
    );
}

/// Would this check ever fire in this repo? Display only — the hooks scope on
/// staged files and the nearest manifest, so this answers "ever", not "now".
fn applies_here(r: &Repo, check: &str) -> bool {
    crate::checks::rollup(std::slice::from_ref(r))
        .into_iter()
        .find(|rollup| rollup.name == check)
        .map(|rollup| rollup.applicable > 0)
        .unwrap_or(false)
}

fn detail(f: &mut Frame, area: Rect, app: &App) {
    let rows = app.rows();
    let Some(repo) = rows.get(app.selected) else {
        return;
    };
    let mut lines = vec![
        Line::from(repo.path.to_string_lossy().into_owned()),
        Line::from(format!(
            "{} · {} · bake {}",
            if repo.managed { "managed" } else { "unmanaged" },
            if repo.languages.is_empty() {
                "no manifest".to_string()
            } else {
                repo.languages.join(" ")
            },
            bake_word(&repo.baked)
        )),
        Line::from(""),
        Line::from("DISPATCHERS").style(tint(app.color, ACCENT)),
    ];
    // Where the hooks are, always — not only when something is wrong with it.
    // A reader who cannot see the directory cannot tell a repo we declined to
    // touch from one that had nothing to do.
    lines.push(Line::from(format!(
        "  {:<20} {}",
        "hooks dir",
        amont_runtime::ui::sanitize(&repo.hooks_dir.describe())
    )));
    if let Some(owner) = &repo.shares_hooks_with {
        lines.push(Line::from(format!(
            "  {:<20} covered by {}",
            "shares hooks",
            amont_runtime::ui::sanitize_path(owner)
        )));
    }
    for (i, n) in DISPATCHERS.iter().enumerate() {
        let s = match repo.shims.get(i) {
            Some(ShimState::Ok { baked }) => format!("ok       -> {baked}"),
            Some(ShimState::Drifted) => "DRIFTED  does not match the template".into(),
            Some(ShimState::Symlink { target }) => format!(
                "SYMLINK  -> {}  (a write here would rewrite that file)",
                target
                    .as_deref()
                    .map(amont_runtime::ui::sanitize_path)
                    .unwrap_or_else(|| "?".to_string())
            ),
            Some(ShimState::Unreadable { why }) => {
                format!("UNKNOWN  {}", amont_runtime::ui::sanitize(why))
            }
            _ => "MISSING".to_string(),
        };
        lines.push(Line::from(format!("  {n:<20} {s}")));
    }
    if !repo.stale_ours.is_empty() || repo.hook_pkgjson {
        lines.push(Line::from(""));
        lines.push(
            Line::from("LEFTOVERS OF OURS (nothing dispatches these — fix removes them)")
                .style(tint(app.color, ACCENT)),
        );
        for name in &repo.stale_ours {
            lines.push(Line::from(format!("  {name}")));
        }
        if repo.hook_pkgjson {
            lines.push(Line::from("  package.json (node era)"));
        }
    }
    // A separate block, and the heading is the whole point: these are hooks
    // somebody else wrote, `fix` LEAVES THEM ALONE, and for two releases it
    // silently deleted them while listing them under the same "LEFTOVERS"
    // heading as our own retired shims.
    if !repo.foreign_subs.is_empty() {
        lines.push(Line::from(""));
        lines.push(
            Line::from("NOT OURS (left alone — nothing dispatches these either)")
                .style(tint(app.color, ACCENT)),
        );
        for name in &repo.foreign_subs {
            lines.push(Line::from(format!("  {name}")));
        }
    }
    if !repo.skips.is_empty() {
        lines.push(Line::from(""));
        lines.push(Line::from("hook.skip").style(tint(app.color, ACCENT)));
        for skip in &repo.skips {
            // What it COSTS, not what it says: a trigger silences fifteen and
            // a typo silences none, and the config line looks the same either
            // way.
            let scope = match &skip.scope {
                crate::skips::Scope::Local => "local",
                crate::skips::Scope::Global => "global",
                crate::skips::Scope::Other { .. } => "other",
            };
            // The verdict goes FIRST. Appending it after the names put the
            // warning past the right edge of the terminal for exactly the
            // values that needed it — nineteen check names is a long line, and
            // the alarm was the first thing truncated.
            let head = if skip.is_inert() {
                // The shape a mistake takes now that naming is exact. Under
                // substring matching almost any string hit something, so this
                // case was rare; it is the common one for a typo.
                "! names no check".to_string()
            } else if skip.is_trigger() {
                format!(
                    "the whole {} trigger — {} checks",
                    skip.value,
                    skip.suppresses.len()
                )
            } else {
                skip.suppresses.join(", ")
            };
            lines.push(Line::from(format!(
                "  {:<22} {scope:<7} -> {head}",
                skip.value
            )));
            // Names on their own line, where truncation costs detail rather
            // than the warning.
            if skip.suppresses.len() > 1 {
                lines.push(Line::from(format!(
                    "  {:<22} {:<7}    {}",
                    "",
                    "",
                    skip.suppresses.join(", ")
                )));
            }
        }
    }
    if !repo.severities.is_empty() {
        lines.push(Line::from(""));
        lines.push(Line::from("amont.severity").style(tint(app.color, ACCENT)));
        for entry in &repo.severities {
            let scope = match &entry.scope {
                crate::skips::Scope::Local => "local",
                crate::skips::Scope::Global => "global",
                crate::skips::Scope::Other { .. } => "other",
            };
            // Three outcomes worth telling apart, and only one of them is what
            // the author probably thought they were writing.
            let head = if entry.shadowed() {
                // Configured, and overridden by a later entry. Saying "does NOT
                // block" here would be the dashboard contradicting the
                // dispatcher, which is the one thing this block must not do.
                // Which entry wins is git's precedence, not ours to name — an
                // include can beat a local. The other row for this check is the
                // one that applies, and it is listed right here.
                "overridden — another entry for this check is the one git applies".to_string()
            } else if entry.is_inert() {
                // Silent no-op. Git accepts any key and any value here, so a
                // typo leaves a line in the config that looks like policy and
                // enforces the default.
                "! changes nothing — unknown check or value".to_string()
            } else if entry.weakens() {
                "runs and reports, does NOT block".to_string()
            } else {
                "blocks (the default, written out)".to_string()
            };
            lines.push(Line::from(format!(
                "  {:<22} {:<7} {:<6} -> {head}",
                entry.check, scope, entry.value
            )));
        }
    }
    if !repo.declared.is_empty() {
        lines.push(Line::from(""));
        // Trust state goes in the HEADING, not beside each check: it is a
        // property of the file, and repeating it per row would read as though
        // some of them were running.
        let heading = match repo.trusted {
            Some(true) => format!("{DECLARED_FILE} (declared here, trusted)"),
            Some(false) => {
                format!("{DECLARED_FILE} (declared here — NOT TRUSTED, none of these run)")
            }
            None => format!("{DECLARED_FILE} (declared here)"),
        };
        lines.push(Line::from(heading).style(tint(
            app.color,
            if repo.trusted == Some(false) {
                Color::Red
            } else {
                ACCENT
            },
        )));
        for declared in &repo.declared {
            match &declared.state {
                // The verdict first, as in the hook.skip block: a long command
                // must not push the reason it never runs off the right edge.
                crate::scan::DeclaredState::Unusable { why } => lines.push(
                    Line::from(format!("  {:<18} ! {why}", declared.name))
                        .style(tint(app.color, Color::Red)),
                ),
                crate::scan::DeclaredState::Usable { exts, command, .. } => {
                    lines.push(Line::from(format!(
                        "  {:<18} {:<6} {:<10} {}",
                        declared.name,
                        declared.stage,
                        scope_of(exts),
                        command
                    )))
                }
            }
        }
    }
    if repo.bypasses.total > 0 {
        lines.push(Line::from(""));
        lines.push(
            Line::from("unverified commits (no record their commit-time gate ran)")
                .style(tint(app.color, ACCENT)),
        );
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or_default();
        for sc in &repo.bypasses.by_script {
            lines.push(Line::from(format!(
                "  {:<18} {:>3}   last {}",
                sc.script,
                sc.count,
                amont_runtime::bypass::age(now, sc.last)
            )));
        }
    }
    lines.push(Line::from(""));
    let (agents_md_word, agents_md_colour) = match repo.agents_md {
        AgentsMdState::UpToDate => ("up to date".to_string(), Color::Green),
        // Opt-in, so absence is not a problem — matches the neutral colour
        // `declared`'s own "none" case would get, not the red a real drift
        // does.
        AgentsMdState::Missing => ("not present (opt-in)".to_string(), Color::Reset),
        AgentsMdState::Drifted => ("drifted from the generated block".to_string(), Color::Red),
        AgentsMdState::Malformed => (
            "unpaired marker — fix or remove it by hand".to_string(),
            Color::Red,
        ),
    };
    lines.push(
        Line::from(format!("AGENTS.md: {agents_md_word}")).style(tint(app.color, agents_md_colour)),
    );
    lines.push(Line::from(""));
    lines.push(
        Line::from(format!("CHECKS ({})", crate::checks::all_checks().len()))
            .style(tint(app.color, ACCENT)),
    );
    // Windowed around the cursor. The full list is twenty lines plus a legend,
    // which on a short terminal pushed the hook.skip diagnostic off screen —
    // the section a reader most needs was the one that disappeared.
    let all = crate::checks::all_checks();
    let room = (area.height as usize)
        .saturating_sub(lines.len() + 3)
        .max(3);
    let first = app
        .check_selected
        .saturating_sub(room / 2)
        .min(all.len().saturating_sub(room));
    for (i, check) in all.iter().copied().enumerate().skip(first).take(room) {
        let skipped = repo
            .skips
            .iter()
            .any(|skip| amont_runtime::skip_suppresses(check, &skip.value));
        // Three states, three glyphs, three words. A check that is correctly
        // silent must never look like a broken one.
        let (glyph, word) = if skipped {
            ('⊘', "skipped")
        } else if applies_here(repo, check) {
            ('●', "runs")
        } else {
            ('○', "inert")
        };
        let cursor = if i == app.check_selected { '>' } else { ' ' };
        lines.push(Line::from(format!(
            "{cursor} {glyph} {:<22} {:<11} {word}",
            amont_runtime::short_name(check),
            crate::checks::trigger_of(check),
        )));
    }
    lines.push(Line::from(
        "  ● runs here   ○ inert (no matching manifest)   ⊘ skipped via hook.skip",
    ));

    f.render_widget(Paragraph::new(lines), area);
}

/// The transposed matrix: checks down the side, repo counts across.
///
/// Answers "where does this check actually apply?", which the old text output
/// could not. `APPLICABLE = ACTIVE + SKIPPED`, and INERT is counted separately
/// because a check that is correctly silent is not a problem — conflating the
/// two would invent ninety false problems out of the Rust checks alone.
fn hooks_view(f: &mut Frame, area: Rect, app: &App) {
    let rows = checks::rollup(&app.scan.repos);
    let managed = app.scan.managed_seen;
    let body: Vec<Row> = rows
        .iter()
        .map(|r| {
            // A check that can never fire anywhere is either dead or
            // misconfigured, and that is invisible in a plain list.
            let flag = if r.applicable == 0 { "  <- never" } else { "" };
            Row::new(vec![
                // The SHORT name, with the trigger beside it. Repeating
                // `pre-commit-` down twenty rows spends eleven columns saying
                // nothing, and the thing it hides — that two rows can be the
                // same check on different triggers — is exactly what the
                // reader needs to see.
                Cell::from(format!("{}{flag}", amont_runtime::short_name(r.name))),
                Cell::from(r.trigger),
                Cell::from(format!("{}/{managed}", r.applicable)),
                Cell::from(r.active.to_string()),
                Cell::from(r.skipped.to_string()),
                Cell::from(r.inert.to_string()),
            ])
        })
        .collect();
    f.render_widget(
        Table::new(
            body,
            [
                Constraint::Min(22),
                Constraint::Length(11),
                Constraint::Length(12),
                Constraint::Length(8),
                Constraint::Length(9),
                Constraint::Length(7),
            ],
        )
        .header(
            Row::new(vec![
                "CHECK",
                "TRIGGER",
                "APPLICABLE",
                "ACTIVE",
                "SKIPPED",
                "INERT",
            ])
            .style(tint(app.color, ACCENT)),
        ),
        area,
    );
}

fn footer(f: &mut Frame, area: Rect, app: &App) {
    let rows = app.rows().len();
    let total = app.scan.repos.len();
    let left = if app.mode == Mode::Filter {
        format!("/{}", app.filter.as_str())
    } else if app.filter.is_empty() {
        format!("{rows} rows")
    } else {
        // The match count is always visible while filtering, so an empty result
        // is legible as "the filter excluded everything", not as "nothing here".
        format!("{rows} of {total} rows match {:?}", app.filter.as_str())
    };
    if let Some(n) = &app.notice {
        f.render_widget(Paragraph::new(Line::from(n.clone())), area);
        return;
    }
    let keys = if app.mode == Mode::HookView {
        "h fleet  q quit"
    } else if app.mode == Mode::Detail {
        "j/k check  s skip  u undo  esc back  q quit"
    } else {
        "j/k move  enter detail  / filter  h hooks  esc clear  q quit"
    };
    f.render_widget(Paragraph::new(Line::from(format!("{left}   {keys}"))), area);
}

/// Run the dashboard. Scanning happens on a worker thread so the UI thread
/// never blocks: the scan takes ~7s on a real fleet, well past the point at
/// which an interface stops feeling responsive, and `q` has to work throughout.
pub fn run(root: std::path::PathBuf, depth: usize, binary: String) -> std::io::Result<()> {
    use crossterm::event::{self, Event, KeyCode, KeyEventKind};
    use crossterm::terminal::{
        disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
    };
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    enum Msg {
        Visited(usize),
        /// A repository, sent as it is found. The spec calls streaming
        /// mandatory rather than an optimisation: a spinner over a blank screen
        /// for seven seconds is not "visibility of system status".
        Found(Box<Repo>),
        Done(Box<FleetScan>),
    }

    let (tx, rx) = mpsc::channel();
    let scan_root = root.clone();
    std::thread::spawn(move || {
        let t = tx.clone();
        let scan = crate::scan::scan(&scan_root, depth, &binary, &mut |p| match p {
            // Throttled: a message per directory would spend more time in the
            // channel than in the walk.
            crate::scan::Progress::Visited { count, .. } if count % 200 == 0 => {
                let _ = t.send(Msg::Visited(count));
            }
            crate::scan::Progress::Found(r) => {
                let _ = t.send(Msg::Found(Box::new(r.clone())));
            }
            _ => {}
        });
        let _ = tx.send(Msg::Done(Box::new(scan)));
    });

    enable_raw_mode()?;
    let mut out = std::io::stdout();
    crossterm::execute!(out, EnterAlternateScreen)?;
    let mut term = Terminal::new(CrosstermBackend::new(out))?;

    let started = Instant::now();
    let mut app = App::new(FleetScan {
        root,
        depth,
        git_dirs_found: 0,
        hook_dirs_seen: 0,
        managed_seen: 0,
        unmanaged_seen: 0,
        unreadable: Vec::new(),
        hooks_outside_seen: 0,
        excluded_dirs: 0,
        dirs_visited: 0,
        bypassed_commits: 0,
        repos_with_bypasses: 0,
        repos: Vec::new(),
    });
    app.scanning = true;

    let result = loop {
        while let Ok(msg) = rx.try_recv() {
            match msg {
                Msg::Visited(n) => app.visited = n,
                Msg::Found(r) => {
                    // Counters are recomputed from what has arrived, so the
                    // header never shows a denominator it cannot justify.
                    app.scan.repos.push(*r);
                    app.scan.git_dirs_found = app.scan.repos.len();
                    app.scan.managed_seen = app.scan.repos.iter().filter(|r| r.managed).count();
                    app.scan.unmanaged_seen = app.scan.git_dirs_found - app.scan.managed_seen;
                }
                Msg::Done(s) => {
                    app.scan = *s;
                    app.scanning = false;
                }
            }
        }
        app.elapsed = started.elapsed().as_secs_f64();
        if let Err(e) = term.draw(|f| draw(f, &app)) {
            break Err(e);
        }
        // 16ms: one frame. Long enough not to spin, short enough that a
        // keystroke never feels dropped.
        if event::poll(Duration::from_millis(16))? {
            if let Event::Key(k) = event::read()? {
                if k.kind == KeyEventKind::Press {
                    // crossterm's key codes map straight onto ours; arrows are
                    // their own variants rather than being spelled as j/k, so
                    // they keep working inside a text prompt.
                    let mapped = match k.code {
                        KeyCode::Char(c) => Some(Key::Char(c)),
                        KeyCode::Enter => Some(Key::Enter),
                        KeyCode::Esc => Some(Key::Esc),
                        KeyCode::Backspace => Some(Key::Backspace),
                        KeyCode::Down => Some(Key::Down),
                        KeyCode::Up => Some(Key::Up),
                        _ => None,
                    };
                    if let Some(key) = mapped {
                        app.on_key(key);
                    }
                }
            }
        }
        if app.quit {
            break Ok(());
        }
    };

    disable_raw_mode()?;
    crossterm::execute!(term.backend_mut(), LeaveAlternateScreen)?;
    term.show_cursor()?;
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;
    use std::path::PathBuf;

    fn render(app: &App, w: u16, h: u16) -> String {
        let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
        term.draw(|f| draw(f, app)).unwrap();
        let buf = term.backend().buffer().clone();
        (0..buf.area.height)
            .map(|y| {
                (0..buf.area.width)
                    .map(|x| buf[(x, y)].symbol().to_string())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn empty_scan() -> FleetScan {
        FleetScan {
            root: PathBuf::from("/Users/me/Dev"),
            depth: 6,
            git_dirs_found: 0,
            hook_dirs_seen: 0,
            managed_seen: 0,
            unmanaged_seen: 0,
            unreadable: Vec::new(),
            hooks_outside_seen: 0,
            excluded_dirs: 0,
            dirs_visited: 412,
            bypassed_commits: 0,
            repos_with_bypasses: 0,
            repos: Vec::new(),
        }
    }

    fn repo(path: &str, managed: bool) -> Repo {
        Repo {
            path: PathBuf::from(path),
            managed,
            shims: vec![
                ShimState::Ok {
                    baked: "/bin/gh".into()
                };
                4
            ],
            baked: BakeState::Current,
            stale_ours: Vec::new(),
            foreign_subs: Vec::new(),
            hook_pkgjson: false,
            languages: vec!["rust".into()],
            applicable: Vec::new(),
            skips: Vec::new(),
            severities: Vec::new(),
            bypasses: crate::bypasses::Bypasses::default(),
            declared: Vec::new(),
            trusted: None,
            agents_md: AgentsMdState::Missing,
            hooks_dir: crate::scan::HooksDir::In {
                path: std::path::PathBuf::from(".git/hooks"),
            },
            shares_hooks_with: None,
        }
    }

    fn scan_with_repos(rs: Vec<Repo>) -> FleetScan {
        let managed = rs.iter().filter(|repo| repo.managed).count();
        FleetScan {
            root: PathBuf::from("/root"),
            depth: 6,
            git_dirs_found: rs.len(),
            hook_dirs_seen: rs.len(),
            managed_seen: managed,
            unmanaged_seen: rs.len() - managed,
            unreadable: Vec::new(),
            hooks_outside_seen: 0,
            excluded_dirs: 3,
            dirs_visited: 100,
            bypassed_commits: 0,
            repos_with_bypasses: 0,
            repos: rs,
        }
    }

    /// THE success criterion, from the spec: a broken scan must say so rather
    /// than render a clean, empty, green fleet. This is the whole reason the
    /// tool was worth building, so it is a test and not a promise.
    #[test]
    fn an_empty_scan_renders_scan_failure() {
        let out = render(&App::new(empty_scan()), 90, 16);
        assert!(out.contains("SCAN FAILURE"), "{out}");
        assert!(out.contains("--root"), "must name what to check: {out}");
        assert!(out.contains("--depth"), "{out}");
        assert!(
            out.contains("/Users/me/Dev"),
            "and show the values used: {out}"
        );
        assert!(out.contains("412"), "and what it did look at: {out}");
    }

    /// The inverse: a healthy fleet must NOT shout failure.
    #[test]
    fn a_populated_scan_does_not_claim_failure() {
        let out = render(&App::new(scan_with_repos(vec![repo("a", true)])), 90, 16);
        assert!(!out.contains("SCAN FAILURE"), "{out}");
    }

    /// Counts carry denominators. `4/4`, never "consistent".
    #[test]
    fn the_consistency_band_shows_a_denominator() {
        let app = App::new(scan_with_repos(vec![repo("a", true), repo("b", true)]));
        let out = render(&app, 110, 16);
        assert!(out.contains("2/2"), "expected N/M per hook: {out}");
        assert!(out.contains("repositories"), "{out}");
    }

    /// Colour comes from the terminal's palette, not from a fixed cube.
    ///
    /// ratatui's named colours emit base ANSI, which a terminal theme remaps.
    /// Asserting the NAMED colour is the closest a test can get to "follows
    /// your theme" — the actual hue is the terminal's business, which is the
    /// entire point.
    #[test]
    fn state_is_tinted_with_a_named_colour() {
        let mut bad = repo("x", true);
        bad.shims[1] = ShimState::Drifted;
        let mut app = App::new(scan_with_repos(vec![repo("ok", true), bad]));
        app.color = true; // explicit: never depend on the ambient environment

        let mut term = Terminal::new(ratatui::backend::TestBackend::new(110, 12)).unwrap();
        term.draw(|f| draw(f, &app)).unwrap();
        let buf = term.backend().buffer().clone();

        // Color is not Ord, so a Vec rather than a set.
        let used: Vec<Color> = (0..buf.area.height)
            .flat_map(|y| (0..buf.area.width).map(move |x| (x, y)))
            .map(|(x, y)| buf[(x, y)].fg)
            .collect();
        assert!(used.contains(&Color::Green), "an ok row should be green");
        assert!(used.contains(&Color::Red), "a drifted row should be red");
        // Nothing from the fixed 256-colour cube, which would ignore the theme.
        assert!(
            !used
                .iter()
                .any(|c| matches!(c, Color::Indexed(n) if *n > 15)),
            "a fixed palette entry overrides the user's theme"
        );
    }

    /// The selection must not erase the status of the row it is on.
    ///
    /// A row style with `fg` overrides the per-cell colour, so accenting the
    /// selected row hid whether it was ok or drifted — on exactly the row being
    /// read. Reverse video swaps the cell's own colour instead.
    #[test]
    fn the_selected_row_keeps_its_status_colour() {
        let mut app = App::new(scan_with_repos(vec![repo("ok", true), repo("b", true)]));
        app.color = true;
        app.selected = 0;

        let mut term = Terminal::new(ratatui::backend::TestBackend::new(110, 12)).unwrap();
        term.draw(|f| draw(f, &app)).unwrap();
        let buf = term.backend().buffer().clone();
        let used: Vec<Color> = (0..buf.area.height)
            .flat_map(|y| (0..buf.area.width).map(move |x| (x, y)))
            .map(|(x, y)| buf[(x, y)].fg)
            .collect();
        assert!(
            used.contains(&Color::Green),
            "the selected ok row must still read as ok"
        );
        assert!(used.contains(&ACCENT), "and the cursor carries the accent");
    }

    /// Headers use the accent, which is a slot rather than a hue — the terminal
    /// decides what cyan looks like.
    #[test]
    fn headers_carry_the_accent() {
        let mut app = App::new(scan_with_repos(vec![repo("a", true)]));
        app.color = true;
        let mut term = Terminal::new(ratatui::backend::TestBackend::new(110, 14)).unwrap();
        term.draw(|f| draw(f, &app)).unwrap();
        let buf = term.backend().buffer().clone();
        // The column header row.
        // Find the rows by CONTENT, not by counting. Counting accented rows
        // passed even with the column header un-accented, because the title
        // line alone satisfied it — an assertion that could not fail.
        let row_text = |y: u16| -> String {
            (0..buf.area.width)
                .map(|x| buf[(x, y)].symbol())
                .collect::<String>()
        };
        let accented = |y: u16| (0..buf.area.width).any(|x| buf[(x, y)].fg == ACCENT);

        let header_y = (0..buf.area.height)
            .find(|y| row_text(*y).contains("REPO") && row_text(*y).contains("STATE"))
            .expect("column header row");
        assert!(accented(header_y), "the column header must be accented");

        let title_y = (0..buf.area.height)
            .find(|y| row_text(*y).contains("/root"))
            .expect("title row");
        assert!(accented(title_y), "the title line must be accented");
    }

    /// With colour off, the selection is still marked — by the symbol and by
    /// reverse video. An accent that vanishes must not take the cursor with it.
    #[test]
    fn the_selection_survives_no_color() {
        let mut app = App::new(scan_with_repos(vec![repo("a", true), repo("b", true)]));
        app.color = false;
        app.selected = 1;

        let mut term = Terminal::new(ratatui::backend::TestBackend::new(110, 12)).unwrap();
        term.draw(|f| draw(f, &app)).unwrap();
        let buf = term.backend().buffer().clone();

        let reversed = (0..buf.area.height)
            .flat_map(|y| (0..buf.area.width).map(move |x| (x, y)))
            .any(|(x, y)| buf[(x, y)].modifier.contains(Modifier::REVERSED));
        assert!(reversed, "reverse video marks the row without colour");

        // Search every row rather than assume a layout offset.
        let screen: String = (0..buf.area.height)
            .map(|y| {
                (0..buf.area.width)
                    .map(|x| buf[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            screen.contains('>'),
            "the cursor symbol is there:\n{screen}"
        );

        let coloured = (0..buf.area.height)
            .flat_map(|y| (0..buf.area.width).map(move |x| (x, y)))
            .any(|(x, y)| buf[(x, y)].fg != Color::Reset);
        assert!(!coloured, "NO_COLOR must leave no foreground colour set");
    }

    #[test]
    fn narrow_terminals_drop_columns_rather_than_scroll() {
        let app = App::new(scan_with_repos(vec![repo("some/repo", true)]));
        let wide = render(&app, 110, 12);
        assert!(wide.contains("LANG") && wide.contains("SKIPS"), "{wide}");
        let mid = render(&app, 80, 12);
        assert!(!mid.contains("LANG"), "LANG should be dropped first: {mid}");
        assert!(mid.contains("SHIMS"), "{mid}");
        let narrow = render(&app, 50, 12);
        assert!(!narrow.contains("SHIMS"), "{narrow}");
        assert!(
            narrow.contains("REPO") && narrow.contains("STATE"),
            "{narrow}"
        );
    }

    /// State is legible without colour: the glyph column and a word.
    #[test]
    fn state_is_encoded_in_text_not_only_colour() {
        let mut r = repo("x", true);
        r.shims[2] = ShimState::Missing;
        let out = render(&App::new(scan_with_repos(vec![r])), 110, 12);
        assert!(out.contains("missing"), "a word, not just a colour: {out}");
        assert!(out.contains('○'), "and a distinct glyph: {out}");
    }

    #[test]
    fn an_unmanaged_repo_is_labelled_not_hidden() {
        let out = render(
            &App::new(scan_with_repos(vec![repo("data/repo", false)])),
            110,
            12,
        );
        assert!(out.contains("unmanaged"), "{out}");
    }

    #[test]
    fn detail_lists_every_dispatcher() {
        let mut app = App::new(scan_with_repos(vec![repo("a", true)]));
        app.mode = Mode::Detail;
        let out = render(&app, 100, 20);
        for n in DISPATCHERS {
            assert!(out.contains(n), "detail must list {n}: {out}");
        }
    }

    /// A trigger is the only way a single value reaches many checks now, and
    /// the detail view names it as such — the cost, not just the value.
    ///
    /// This used to be `t`, which cost nineteen checks by accident of substring
    /// reach and had to be flagged "probably not intended". A trigger IS
    /// intended, so it is reported rather than alarmed about.
    #[test]
    fn detail_shows_what_each_skip_actually_suppresses() {
        let mut r = repo("a", true);
        r.skips = vec![crate::skips::for_test("pre-commit")];
        let mut app = App::new(scan_with_repos(vec![r]));
        app.mode = Mode::Detail;
        let out = render(&app, 120, 30);
        assert!(out.contains("hook.skip"), "{out}");
        assert!(
            out.contains("the whole pre-commit trigger"),
            "must name what it covers: {out}"
        );
        assert!(out.contains("16 checks"), "and how many: {out}");
    }

    /// The shape a mistake takes now: a value that names nothing at all.
    #[test]
    fn detail_flags_a_skip_that_names_nothing() {
        let mut r = repo("a", true);
        r.skips = vec![crate::skips::for_test("t")];
        let mut app = App::new(scan_with_repos(vec![r]));
        app.mode = Mode::Detail;
        let out = render(&app, 120, 30);
        assert!(out.contains("names no check"), "{out}");
    }

    /// A downgraded check is the case the dashboard was blind to: it runs, it
    /// prints, the commit passes, and every column read "ok". The detail view
    /// has to say what the config line does, because the config line does not.
    #[test]
    fn detail_says_a_downgraded_check_does_not_block() {
        let mut r = repo("a", true);
        r.severities = vec![crate::severities::for_test("pre-commit-clippy", "warn")];
        let mut app = App::new(scan_with_repos(vec![r]));
        app.mode = Mode::Detail;
        let out = render(&app, 120, 30);
        assert!(out.contains("amont.severity"), "{out}");
        assert!(out.contains("does NOT block"), "{out}");
        assert!(
            !out.contains("changes nothing"),
            "it does change things: {out}"
        );
    }

    /// Git accepts any key and any value under this section, so the two ways of
    /// configuring nothing look exactly like the way that works.
    #[test]
    fn detail_flags_a_severity_line_that_does_nothing() {
        for entry in [
            crate::severities::for_test("pre-commit-clipy", "warn"),
            crate::severities::for_test("pre-commit-clippy", "advisory"),
        ] {
            let mut r = repo("a", true);
            r.severities = vec![entry.clone()];
            let mut app = App::new(scan_with_repos(vec![r]));
            app.mode = Mode::Detail;
            let out = render(&app, 120, 30);
            assert!(
                out.contains("changes nothing"),
                "{entry:?} was not flagged as inert: {out}"
            );
        }
    }

    /// An explicit `block` is the default written out. Showing it is right;
    /// alarming about it is not.
    #[test]
    fn an_explicit_block_is_shown_without_alarm() {
        let mut r = repo("a", true);
        r.severities = vec![crate::severities::for_test("pre-commit-clippy", "block")];
        let mut app = App::new(scan_with_repos(vec![r]));
        app.mode = Mode::Detail;
        let out = render(&app, 120, 30);
        assert!(out.contains("amont.severity"), "{out}");
        assert!(!out.contains("does NOT block"), "{out}");
        assert!(!out.contains("changes nothing"), "{out}");
    }

    /// The table counts only real downgrades. An inert line must not inflate
    /// the number, or the column becomes noise nobody acts on.
    #[test]
    fn the_bypass_column_is_a_dash_for_a_clean_repo() {
        let app = App::new(scan_with_repos(vec![repo("clean", true)]));
        let out = render(&app, 130, 12);
        assert!(out.contains("BYPASS"), "the column must exist: {out}");
        let row = out
            .lines()
            .find(|l| l.contains("clean "))
            .expect("the repo row");
        assert!(row.contains(" - "), "zero renders as a dash: {row}");
    }

    #[test]
    fn the_bypass_column_counts_and_the_detail_pane_names_the_scripts() {
        let mut r = repo("dodgy", true);
        r.bypasses = crate::bypasses::Bypasses {
            total: 3,
            last: Some(0),
            by_script: vec![
                crate::bypasses::ScriptCount {
                    script: "typecheck".into(),
                    count: 2,
                    last: 0,
                },
                crate::bypasses::ScriptCount {
                    script: "test".into(),
                    count: 1,
                    last: 0,
                },
            ],
        };
        let mut app = App::new(scan_with_repos(vec![r]));
        app.mode = Mode::Browse;
        let out = render(&app, 130, 12);
        let row = out
            .lines()
            .find(|l| l.contains("dodgy "))
            .expect("the repo row");
        assert!(row.contains(" 3 "), "the count renders: {row}");
        app.mode = Mode::Detail;
        let detail = render(&app, 130, 40);
        assert!(detail.contains("unverified commits"), "{detail}");
        assert!(detail.contains("typecheck"), "{detail}");
        assert!(detail.contains("test"), "{detail}");
    }

    #[test]
    fn the_bypass_column_drops_with_the_other_wide_columns() {
        let app = App::new(scan_with_repos(vec![repo("some/repo", true)]));
        let mid = render(&app, 80, 12);
        assert!(
            !mid.contains("BYPASS"),
            "BYPASS drops with the wide columns: {mid}"
        );
    }

    #[test]
    fn the_header_says_nothing_about_bypasses_when_there_are_none() {
        let app = App::new(scan_with_repos(vec![repo("clean", true)]));
        let out = render(&app, 130, 12);
        assert!(
            !out.contains("unverified"),
            "a clean fleet's header stays clean: {out}"
        );
        let mut scan = scan_with_repos(vec![repo("dodgy", true)]);
        scan.bypassed_commits = 7;
        scan.repos_with_bypasses = 3;
        let app = App::new(scan);
        let out = render(&app, 130, 12);
        assert!(out.contains("7 unverified commits in 3 repos"), "{out}");
    }

    #[test]
    fn the_warn_column_counts_only_what_actually_weakens() {
        let mut r = repo("a", true);
        r.severities = vec![
            crate::severities::for_test("pre-commit-clippy", "warn"),
            crate::severities::for_test("pre-commit-prettier", "block"),
            crate::severities::for_test("nonsense", "warn"),
        ];
        let mut app = App::new(scan_with_repos(vec![r]));
        app.mode = Mode::Browse;
        let out = render(&app, 130, 12);
        assert!(out.contains("WARN"), "the column must exist: {out}");
        let row = out
            .lines()
            .find(|l| l.contains("a "))
            .expect("the repo row");
        // Three lines configured, one of which weakens anything.
        assert!(
            row.contains(" 1 "),
            "expected a count of 1 downgrade, got: {row}"
        );
    }

    fn declared(name: &str, broken: Option<&str>) -> crate::scan::DeclaredCheck {
        use crate::scan::{DeclaredCheck, DeclaredState};
        DeclaredCheck {
            name: name.to_string(),
            stage: "pre-commit".to_string(),
            state: match broken {
                Some(why) => DeclaredState::Unusable {
                    why: why.to_string(),
                },
                None => DeclaredState::Usable {
                    severity: "block".to_string(),
                    exts: vec![".sh".to_string()],
                    command: "make lint".to_string(),
                },
            },
        }
    }

    /// The fleet view could not see declared checks at all: a repo could run a
    /// command on every commit that no column mentioned.
    #[test]
    fn detail_lists_what_the_repo_declares() {
        let mut r = repo("a", true);
        r.declared = vec![declared("shellcheck", None)];
        let mut app = App::new(scan_with_repos(vec![r]));
        app.mode = Mode::Detail;
        let out = render(&app, 120, 30);
        assert!(out.contains("amont.conf"), "{out}");
        assert!(out.contains("shellcheck"), "{out}");
        assert!(out.contains("make lint"), "the command itself: {out}");
        assert!(out.contains("*.sh"), "and what gates it: {out}");
    }

    /// A line nobody can parse is a check that is not running. The reason goes
    /// FIRST, so a long command cannot push it off the right edge.
    #[test]
    fn detail_leads_with_why_a_declared_check_cannot_run() {
        let mut r = repo("a", true);
        r.declared = vec![declared("shellcheck", Some("line 1: severity \"LOUD\""))];
        let mut app = App::new(scan_with_repos(vec![r]));
        app.mode = Mode::Detail;
        let out = render(&app, 120, 30);
        let row = out
            .lines()
            .find(|l| l.contains("shellcheck"))
            .expect("the row");
        assert!(row.contains("line 1"), "{row}");
        assert!(
            !row.contains("make lint"),
            "a broken line must not read as a command that runs: {row}"
        );
    }

    /// Three declared checks and three declared checks two of which never run
    /// must not render identically.
    #[test]
    fn the_decl_column_separates_broken_from_merely_present() {
        let mut clean = repo("clean", true);
        clean.declared = vec![declared("a", None), declared("b", None)];
        let mut dirty = repo("dirty", true);
        dirty.declared = vec![declared("a", None), declared("b", Some("line 2: nope"))];
        let app = App::new(scan_with_repos(vec![clean, dirty]));
        let out = render(&app, 140, 12);

        assert!(out.contains("DECL"), "the column must exist: {out}");
        let row = |needle: &str| {
            out.lines()
                .find(|l| l.contains(needle))
                .unwrap_or_default()
                .to_string()
        };
        assert!(row("clean").contains(" 2 "), "{}", row("clean"));
        assert!(row("dirty").contains("2!1"), "{}", row("dirty"));
    }

    /// The AGENTS.md column is its own field, not folded into DECL or STATE —
    /// a repo can have every check running and still be missing the pointer.
    #[test]
    fn the_agents_column_shows_each_repos_own_state() {
        let mut current = repo("current", true);
        current.agents_md = AgentsMdState::UpToDate;
        let mut missing = repo("missing", true);
        missing.agents_md = AgentsMdState::Missing;
        let mut drifted = repo("drifted", true);
        drifted.agents_md = AgentsMdState::Drifted;
        let app = App::new(scan_with_repos(vec![current, missing, drifted]));
        let out = render(&app, 140, 12);

        assert!(out.contains("AGENTS"), "the column must exist: {out}");
        let row = |needle: &str| {
            out.lines()
                .find(|l| l.contains(needle))
                .unwrap_or_default()
                .to_string()
        };
        assert!(row("current").contains("ok"), "{}", row("current"));
        assert!(row("missing").contains('-'), "{}", row("missing"));
        assert!(row("drifted").contains("drift"), "{}", row("drifted"));
    }

    #[test]
    fn detail_reports_the_agents_md_state() {
        let mut r = repo("a", true);
        r.agents_md = AgentsMdState::Drifted;
        let mut app = App::new(scan_with_repos(vec![r]));
        app.mode = Mode::Detail;
        let out = render(&app, 120, 30);
        assert!(out.contains("AGENTS.md"), "{out}");
        assert!(out.contains("drifted"), "{out}");
    }

    /// Ninety-six repositories declare nothing. The columns and the pane must
    /// stay silent for them.
    #[test]
    fn a_repo_declaring_nothing_says_nothing() {
        let mut app = App::new(scan_with_repos(vec![repo("a", true)]));
        app.mode = Mode::Detail;
        let out = render(&app, 120, 30);
        assert!(
            !out.contains("amont.conf"),
            "a file that does not exist was mentioned: {out}"
        );
    }

    /// An overridden entry must not read as active policy — that is the
    /// dashboard contradicting the dispatcher.
    #[test]
    fn detail_marks_a_shadowed_entry_rather_than_calling_it_a_downgrade() {
        let mut r = repo("a", true);
        r.severities = vec![
            crate::severities::shadowed_for_test("pre-commit-clippy", "warn"),
            crate::severities::for_test("pre-commit-clippy", "block"),
        ];
        let mut app = App::new(scan_with_repos(vec![r]));
        app.mode = Mode::Detail;
        let out = render(&app, 120, 30);
        assert!(out.contains("overridden"), "{out}");
        assert!(
            !out.contains("does NOT block"),
            "an overridden warn was reported as a live downgrade:\n{out}"
        );
    }

    /// And the column must not count it either.
    #[test]
    fn the_warn_column_ignores_a_shadowed_entry() {
        let mut r = repo("a", true);
        r.severities = vec![
            crate::severities::shadowed_for_test("pre-commit-clippy", "warn"),
            crate::severities::for_test("pre-commit-clippy", "block"),
        ];
        let app = App::new(scan_with_repos(vec![r]));
        let out = render(&app, 140, 12);
        let row = out
            .lines()
            .find(|l| l.contains("a "))
            .expect("the repo row");
        assert!(
            !row.contains(" 1 "),
            "counted an override git does not apply: {row}"
        );
    }

    /// A precise skip is reported without alarm — crying wolf on the correct
    /// case is how a warning stops being read.
    #[test]
    fn an_exact_skip_is_not_flagged() {
        let mut r = repo("a", true);
        r.skips = vec![crate::skips::for_test("pre-commit-clippy")];
        let mut app = App::new(scan_with_repos(vec![r]));
        app.mode = Mode::Detail;
        let out = render(&app, 120, 30);
        assert!(out.contains("pre-commit-clippy"), "{out}");
        assert!(!out.contains("probably not intended"), "{out}");
        assert!(!out.contains("did you mean"), "nothing to correct: {out}");
    }

    /// A short name is a first-class way to name a check now, not an
    /// imprecision to be corrected. The dashboard used to answer it with
    /// "did you mean pre-push-run-tests-js?"; there is nothing to correct.
    #[test]
    fn a_short_name_is_reported_as_the_check_it_names() {
        let mut r = repo("a", true);
        r.skips = vec![crate::skips::for_test("run-tests-js")];
        let mut app = App::new(scan_with_repos(vec![r]));
        app.mode = Mode::Detail;
        let out = render(&app, 120, 30);
        assert!(out.contains("pre-push-run-tests-js"), "{out}");
        assert!(!out.contains("did you mean"), "nothing to correct: {out}");
    }

    /// A real repo on disk, because the toggle writes git config and a mocked
    /// one would only prove the mock agrees with itself.
    fn repo_on_disk(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("tui-toggle-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("r")).unwrap();
        std::process::Command::new("git")
            .args(["init", "-q", "--template=", "."])
            .current_dir(d.join("r"))
            .output()
            .expect("git");
        d
    }

    fn app_on(root: &std::path::Path) -> App {
        let mut sc = scan_with_repos(vec![repo("r", true)]);
        sc.root = root.to_path_buf();
        let mut app = App::new(sc);
        app.mode = Mode::Detail;
        app
    }

    fn index_of(check: &str) -> usize {
        crate::checks::all_checks()
            .iter()
            .position(|c| *c == check)
            .expect("check")
    }

    /// A toggle now writes immediately: there is nothing to confirm, because a
    /// check id names exactly one check. The type-the-name step existed only
    /// because substring matching could silently take four when you asked for
    /// one — see `a_name_that_prefixes_another_reaches_only_itself` in `skips`.
    #[test]
    fn a_toggle_needs_no_confirmation_step() {
        let root = repo_on_disk("no-confirm");
        let mut app = app_on(&root);
        app.check_selected = index_of("pre-commit-lint-js");
        app.on_key(Key::Char('s'));
        assert_eq!(app.mode, Mode::Detail, "it should just write");
        assert_eq!(
            crate::skips::read(&root.join("r"))
                .iter()
                .map(|e| e.value.clone())
                .collect::<Vec<_>>(),
            vec!["pre-commit-lint-js"],
            "and take only the check that was pointed at"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn s_skips_a_single_check_and_u_takes_it_back() {
        let root = repo_on_disk("single");
        let mut app = app_on(&root);
        app.check_selected = index_of("pre-commit-clippy");

        app.on_key(Key::Char('s'));
        assert_eq!(app.mode, Mode::Detail, "no prompt for the common case");
        assert_eq!(
            crate::skips::read(&root.join("r"))
                .into_iter()
                .map(|e| e.value)
                .collect::<Vec<_>>(),
            vec!["pre-commit-clippy"]
        );
        assert!(app.notice.as_deref().unwrap_or("").contains("u to undo"));

        app.on_key(Key::Char('u'));
        assert!(
            crate::skips::read(&root.join("r")).is_empty(),
            "undo restores"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn filtering_shows_the_match_count_against_the_total() {
        let mut app = App::new(scan_with_repos(vec![
            repo("alpha", true),
            repo("beta", true),
            repo("gamma", true),
        ]));
        app.filter.set("bet");
        assert_eq!(app.rows().len(), 1);
        let out = render(&app, 100, 12);
        assert!(
            out.contains("1 of 3"),
            "an empty result must be legible: {out}"
        );
    }

    #[test]
    fn the_hook_view_transposes_and_keeps_a_denominator() {
        let mut app = App::new(scan_with_repos(vec![repo("a", true), repo("b", true)]));
        app.mode = Mode::HookView;
        let out = render(&app, 110, 30);
        assert!(out.contains("APPLICABLE"), "{out}");
        assert!(out.contains("INERT"), "inert must be its own column: {out}");
        assert!(out.contains("2/2"), "counts carry the managed total: {out}");
    }

    /// The detail view's CHECKS panel gets the same treatment, and its trigger
    /// comes from the registry rather than from the front of the id. Reading it
    /// off the string would work today and would be a second opinion about a
    /// check's stage — the moment the two disagreed, this panel would be the one
    /// that was wrong.
    #[test]
    fn the_checks_panel_names_each_trigger() {
        let mut app = App::new(scan_with_repos(vec![repo("a", true)]));
        app.mode = Mode::Detail;
        let out = render(&app, 100, 40);
        let row = |name: &str| {
            out.lines()
                .find(|l| l.contains(name))
                .unwrap_or("")
                .to_string()
        };
        assert!(row("clippy").contains("pre-commit"), "{}", row("clippy"));
        assert!(
            row("branch-protect").contains("pre-push"),
            "a pre-push check must not be labelled pre-commit: {}",
            row("branch-protect")
        );
        assert!(
            !row("clippy").contains("pre-commit-clippy"),
            "the column already says it: {}",
            row("clippy")
        );
    }

    /// The trigger is a COLUMN, not eleven characters repeated down the CHECK
    /// column. Two checks can share a short name — one on each trigger — and
    /// this column is the only thing that tells those rows apart.
    #[test]
    fn the_hook_view_gives_the_trigger_its_own_column() {
        let mut app = App::new(scan_with_repos(vec![repo("a", true)]));
        app.mode = Mode::HookView;
        let out = render(&app, 110, 30);
        assert!(out.contains("TRIGGER"), "{out}");
        assert!(out.contains("clippy"), "{out}");
        assert!(
            !out.contains("pre-commit-clippy"),
            "the trigger column already says it: {out}"
        );

        // Both triggers reach the table, so the column is doing work rather
        // than printing one constant.
        let row = |name: &str| {
            out.lines()
                .find(|l| l.contains(name))
                .unwrap_or("")
                .to_string()
        };
        assert!(row("clippy").contains("pre-commit"), "{}", row("clippy"));
        assert!(
            row("branch-protect").contains("pre-push"),
            "{}",
            row("branch-protect")
        );
    }

    /// A check that applies nowhere is called out, because "0 everywhere" is
    /// invisible in a column of numbers.
    #[test]
    fn a_check_that_applies_nowhere_is_flagged() {
        let mut r = repo("only-js", true);
        r.languages = vec!["js".into()];
        let mut app = App::new(scan_with_repos(vec![r]));
        app.mode = Mode::HookView;
        let out = render(&app, 110, 30);
        assert!(
            out.contains("never"),
            "expected a marker on the dead rows: {out}"
        );
    }

    #[test]
    fn h_toggles_the_hook_view() {
        let mut app = App::new(scan_with_repos(vec![repo("a", true)]));
        assert_eq!(app.mode, Mode::Browse);
        app.on_key(Key::Char('h'));
        assert_eq!(app.mode, Mode::HookView);
        app.on_key(Key::Char('h'));
        assert_eq!(app.mode, Mode::Browse);
    }
    #[test]
    fn keys_move_enter_and_quit() {
        let mut app = App::new(scan_with_repos(vec![repo("a", true), repo("b", true)]));
        app.on_key(Key::Char('j'));
        assert_eq!(app.selected, 1);
        app.on_key(Key::Char('j'));
        assert_eq!(app.selected, 1, "must not run past the end");
        app.on_key(Key::Char('k'));
        assert_eq!(app.selected, 0);
        app.on_key(Key::Enter);
        assert_eq!(app.mode, Mode::Detail);
        app.on_key(Key::Esc);
        assert_eq!(
            app.mode,
            Mode::Browse,
            "esc leaves detail before clearing a filter"
        );
        app.on_key(Key::Char('q'));
        assert!(app.quit);
    }

    /// Arrows are their own variants rather than aliases for j/k, so they still
    /// move the selection but do not type letters into a prompt.
    #[test]
    fn arrows_move_and_do_not_type() {
        let mut app = App::new(scan_with_repos(vec![repo("a", true), repo("b", true)]));
        app.on_key(Key::Down);
        assert_eq!(app.selected, 1);
        app.on_key(Key::Up);
        assert_eq!(app.selected, 0);

        app.on_key(Key::Char('/'));
        app.on_key(Key::Down);
        assert_eq!(
            app.filter.as_str(),
            "",
            "an arrow must not become a character"
        );
    }

    #[test]
    fn filter_mode_captures_typing_and_escape_clears_it() {
        let mut app = App::new(scan_with_repos(vec![repo("alpha", true)]));
        app.on_key(Key::Char('/'));
        assert_eq!(app.mode, Mode::Filter);
        for c in "alp".chars() {
            app.on_key(Key::Char(c));
        }
        assert_eq!(app.filter.as_str(), "alp");
        app.on_key(Key::Backspace);
        assert_eq!(
            app.filter.as_str(),
            "al",
            "backspace edits rather than exits"
        );
        app.on_key(Key::Esc);
        assert_eq!(app.mode, Mode::Browse);
        assert!(app.filter.is_empty());
    }

    /// `q` inside a prompt is a letter, not a command. Losing this is the
    /// classic modal-editor bug.
    #[test]
    fn q_types_rather_than_quits_while_filtering() {
        let mut app = App::new(scan_with_repos(vec![repo("a", true)]));
        app.on_key(Key::Char('/'));
        app.on_key(Key::Char('q'));
        assert!(!app.quit, "q must be text here");
        assert_eq!(app.filter.as_str(), "q");
    }

    /// Enter keeps the filter and returns to the list; Esc discards it. Two
    /// different intentions that a single "leave the prompt" would conflate.
    #[test]
    fn enter_keeps_the_filter_and_esc_discards_it() {
        let mut app = App::new(scan_with_repos(vec![
            repo("alpha", true),
            repo("beta", true),
        ]));
        app.on_key(Key::Char('/'));
        app.on_key(Key::Char('b'));
        app.on_key(Key::Enter);
        assert_eq!(app.mode, Mode::Browse);
        assert_eq!(app.filter.as_str(), "b", "enter commits the filter");
        assert_eq!(app.rows().len(), 1);
    }
}
