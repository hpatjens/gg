use crate::app::{App, Focus, LogFocus, Modal, PushDialog, StashFocus, Tab};
use crate::git::{FileEntry, Stage, StorageMode};
use crate::tree::Row;
use ratatui::{
    Frame,
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Tabs, Wrap},
};
use std::time::Duration;

pub fn render(f: &mut Frame, app: &App) {
    let area = f.area();
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(2),
            Constraint::Length(1),
            Constraint::Min(1),
            Constraint::Length(1),
        ])
        .split(area);

    render_tabs(f, app, chunks[0]);
    render_header(f, app, chunks[1]);
    match app.tab {
        Tab::Status => render_main(f, app, chunks[2]),
        Tab::Stash => render_stash(f, app, chunks[2]),
        Tab::Log => render_log(f, app, chunks[2]),
    }
    render_footer(f, app, chunks[3]);

    match &app.modal {
        Modal::None => {}
        Modal::Commit(_) => render_commit_modal(f, app, area),
        Modal::Push(d) => render_push_modal(f, d, area),
        Modal::Confirm(c) => render_confirm_modal(f, c, area),
    }
}

fn render_tabs(f: &mut Frame, app: &App, area: Rect) {
    let titles = vec![
        Line::from("[1] Status"),
        Line::from("[2] Stash"),
        Line::from("[3] Log"),
    ];
    let selected = match app.tab {
        Tab::Status => 0,
        Tab::Stash => 1,
        Tab::Log => 2,
    };
    let tabs = Tabs::new(titles)
        .block(
            Block::default()
                .borders(Borders::BOTTOM)
                .border_style(Style::default().fg(Color::DarkGray)),
        )
        .select(selected)
        .divider(Span::styled("│", Style::default().fg(Color::DarkGray)))
        .style(Style::default().fg(Color::DarkGray))
        .highlight_style(
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        );
    f.render_widget(tabs, area);
}

fn render_header(f: &mut Frame, app: &App, area: Rect) {
    let up = app.upstream.clone().unwrap_or_else(|| "(none)".into());
    let mut spans = vec![
        Span::raw(" branch: "),
        Span::styled(app.branch.clone(), Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)),
        Span::raw("  upstream: "),
        Span::styled(up, Style::default().fg(Color::Magenta)),
    ];
    match app.tab {
        Tab::Status => {
            let (git_n, lfs_n) = app.staged_counts();
            spans.push(Span::raw(format!("  [staged: {} git, {} LFS]", git_n, lfs_n)));
        }
        Tab::Stash => {
            spans.push(Span::raw(format!("  [{} stashes]", app.stash.stashes.len())));
        }
        Tab::Log => {
            spans.push(Span::raw(format!("  [{} commits]", app.log.commits.len())));
        }
    }
    f.render_widget(Paragraph::new(Line::from(spans)).alignment(Alignment::Right), area);
}

fn render_main(f: &mut Frame, app: &App, area: Rect) {
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(area);
    render_files(f, app, cols[0]);
    render_diff(f, app, cols[1]);
}

fn render_log(f: &mut Frame, app: &App, area: Rect) {
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(area);
    render_log_list(f, app, cols[0]);
    render_log_details(f, app, cols[1]);
}

fn render_log_list(f: &mut Frame, app: &App, area: Rect) {
    let focused = app.log.focus == LogFocus::List;
    let border_style = if focused {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default().fg(Color::DarkGray)
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!(" Commits ({}) ", app.log.commits.len()))
        .border_style(border_style);

    if app.log.commits.is_empty() {
        let p = Paragraph::new("(no commits)").block(block);
        f.render_widget(p, area);
        return;
    }
    let items: Vec<ListItem> = app
        .log
        .commits
        .iter()
        .map(|c| {
            let line = Line::from(vec![
                Span::styled(c.short.clone(), Style::default().fg(Color::Yellow)),
                Span::raw(" "),
                Span::styled(c.date.clone(), Style::default().fg(Color::DarkGray)),
                Span::raw(" "),
                Span::styled(truncate(&c.author, 14), Style::default().fg(Color::Magenta)),
                Span::raw(" "),
                Span::styled(c.subject.clone(), Style::default().fg(Color::White)),
            ]);
            ListItem::new(line)
        })
        .collect();
    let list = List::new(items)
        .block(block)
        .highlight_style(Style::default().bg(Color::DarkGray).add_modifier(Modifier::BOLD))
        .highlight_symbol("> ");
    let mut state = ListState::default();
    state.select(Some(app.log.cursor));
    f.render_stateful_widget(list, area, &mut state);
}

fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        let cut: String = s.chars().take(n.saturating_sub(1)).collect();
        format!("{}…", cut)
    }
}

fn render_stash(f: &mut Frame, app: &App, area: Rect) {
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(area);
    render_stash_list(f, app, cols[0]);
    render_stash_details(f, app, cols[1]);
}

fn render_stash_list(f: &mut Frame, app: &App, area: Rect) {
    let focused = app.stash.focus == StashFocus::List;
    let border_style = if focused {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default().fg(Color::DarkGray)
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!(" Stashes ({}) ", app.stash.stashes.len()))
        .border_style(border_style);

    if app.stash.stashes.is_empty() {
        let p = Paragraph::new("(no stashes)").block(block);
        f.render_widget(p, area);
        return;
    }
    let items: Vec<ListItem> = app
        .stash
        .stashes
        .iter()
        .map(|s| {
            let line = Line::from(vec![
                Span::styled(s.reference.clone(), Style::default().fg(Color::Yellow)),
                Span::raw("  "),
                Span::styled(s.subject.clone(), Style::default().fg(Color::White)),
            ]);
            ListItem::new(line)
        })
        .collect();
    let list = List::new(items)
        .block(block)
        .highlight_style(Style::default().bg(Color::DarkGray).add_modifier(Modifier::BOLD))
        .highlight_symbol("> ");
    let mut state = ListState::default();
    state.select(Some(app.stash.cursor));
    f.render_stateful_widget(list, area, &mut state);
}

fn render_stash_details(f: &mut Frame, app: &App, area: Rect) {
    let focused = app.stash.focus == StashFocus::Details;
    let border_style = if focused {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default().fg(Color::DarkGray)
    };
    let title = match app.stash.stashes.get(app.stash.cursor) {
        Some(s) => format!(" {} ", s.reference),
        None => " Details ".into(),
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .title(title)
        .border_style(border_style);
    let lines: Vec<Line> = app.stash.details_text.lines().map(diff_line).collect();
    let p = Paragraph::new(lines)
        .block(block)
        .wrap(Wrap { trim: false })
        .scroll((app.stash.details_scroll, 0));
    f.render_widget(p, area);
}

fn render_log_details(f: &mut Frame, app: &App, area: Rect) {
    let focused = app.log.focus == LogFocus::Details;
    let border_style = if focused {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default().fg(Color::DarkGray)
    };
    let title = match app.log.commits.get(app.log.cursor) {
        Some(c) => format!(" {} ", c.short),
        None => " Details ".into(),
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .title(title)
        .border_style(border_style);
    let lines: Vec<Line> = app.log.details_text.lines().map(diff_line).collect();
    let p = Paragraph::new(lines)
        .block(block)
        .wrap(Wrap { trim: false })
        .scroll((app.log.details_scroll, 0));
    f.render_widget(p, area);
}

fn status_char_color(r: &Row) -> (char, Color) {
    if r.agg.conflict > 0 {
        return ('U', Color::Red);
    }
    if r.agg.untracked > 0 && r.agg.staged == 0 && r.agg.unstaged == 0 {
        return ('?', Color::Red);
    }
    let letter = if r.is_dir {
        'M'
    } else if r.agg.added > 0 {
        'A'
    } else if r.agg.deleted > 0 {
        'D'
    } else {
        'M'
    };
    let color = if r.agg.unstaged > 0 || r.agg.untracked > 0 {
        if r.agg.staged > 0 { Color::Yellow } else { Color::Red }
    } else {
        Color::Green
    };
    (letter, color)
}

fn row_line(r: &Row, entry: Option<&FileEntry>) -> Line<'static> {
    let (ch, col) = status_char_color(r);
    let warn = if r.lfs_pointer_warn { '!' } else { ' ' };
    let indent: String = "  ".repeat(r.level);
    let arrow = if r.is_dir {
        if r.expanded { "v " } else { "> " }
    } else {
        "  "
    };
    let dim = Style::default().fg(Color::DarkGray);
    let (status_style, name_style, warn_style) = if r.is_dir {
        (
            Style::default().fg(col),
            dim,
            Style::default().fg(Color::Yellow),
        )
    } else {
        (
            Style::default().fg(col).add_modifier(Modifier::BOLD),
            Style::default().fg(Color::White),
            Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD),
        )
    };
    let mut spans = vec![
        Span::styled(ch.to_string(), status_style),
        Span::styled(warn.to_string(), warn_style),
        Span::raw(" "),
        Span::styled(indent, dim),
        Span::styled(arrow.to_string(), dim),
        Span::styled(r.name.clone(), name_style),
    ];
    if r.is_dir {
        spans.push(Span::styled("/", dim));
    } else {
        spans.push(Span::raw(" "));
        spans.extend(storage_badge_spans(r, entry));
    }
    Line::from(spans)
}

fn storage_badge_spans(r: &Row, entry: Option<&FileEntry>) -> Vec<Span<'static>> {
    let lfs_style = Style::default().fg(Color::Yellow);
    let git_style = Style::default().fg(Color::DarkGray);
    let arrow_style = Style::default().fg(Color::White).add_modifier(Modifier::BOLD);
    let bracket = Style::default().fg(Color::DarkGray);

    let mode_span = |m: StorageMode| match m {
        StorageMode::Lfs => Span::styled("LFS", lfs_style),
        StorageMode::Git => Span::styled("git", git_style),
    };

    let e = match entry {
        Some(e) => e,
        None => {
            return vec![
                Span::styled("[", bracket),
                if r.lfs_tracked { Span::styled("LFS", lfs_style) } else { Span::styled("git", git_style) },
                Span::styled("]", bracket),
            ];
        }
    };

    let mut spans = vec![Span::styled("[", bracket)];

    match (e.prev_storage, e.next_storage) {
        (Some(prev), Some(next)) if prev == next => {
            spans.push(mode_span(prev));
        }
        (Some(prev), Some(next)) => {
            spans.push(mode_span(prev));
            spans.push(Span::styled(" → ", arrow_style));
            spans.push(mode_span(next));
        }
        (None, Some(next)) => {
            spans.push(Span::styled("+ ", arrow_style));
            spans.push(mode_span(next));
        }
        (Some(prev), None) if e.index == Stage::Deleted => {
            spans.push(Span::styled("− ", arrow_style));
            spans.push(mode_span(prev));
        }
        (Some(prev), None) => {
            spans.push(mode_span(prev));
        }
        (None, None) => {
            if e.index == Stage::Untracked {
                spans.push(Span::styled("+ ", arrow_style));
                spans.push(if r.lfs_tracked { mode_span(StorageMode::Lfs) } else { mode_span(StorageMode::Git) });
            } else if r.lfs_tracked {
                spans.push(mode_span(StorageMode::Lfs));
            } else {
                spans.push(mode_span(StorageMode::Git));
            }
        }
    }

    spans.push(Span::styled("]", bracket));
    spans
}

fn render_files(f: &mut Frame, app: &App, area: Rect) {
    let focused = matches!(app.focus, Focus::Files);
    let border_style = if focused {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default().fg(Color::DarkGray)
    };
    let title = format!(" Files ({}) ", app.rows.len());
    let block = Block::default()
        .borders(Borders::ALL)
        .title(title)
        .border_style(border_style);

    if app.rows.is_empty() {
        let p = Paragraph::new("clean — nothing to commit").block(block);
        f.render_widget(p, area);
        return;
    }

    let items: Vec<ListItem> = app
        .rows
        .iter()
        .enumerate()
        .map(|(i, r)| {
            let entry = r.entry_index.and_then(|idx| app.entries.get(idx));
            let item = ListItem::new(row_line(r, entry));
            let selected = i == app.cursor;
            let mixed = r.agg.unstaged > 0 || r.agg.untracked > 0;
            let bg = if r.agg.staged > 0 {
                if r.is_dir {
                    if mixed {
                        if selected { Some(Color::DarkGray) } else { None }
                    } else if selected {
                        Some(Color::Rgb(40, 90, 50))
                    } else {
                        Some(Color::Rgb(20, 45, 25))
                    }
                } else if mixed {
                    if selected { Some(Color::Rgb(90, 75, 25)) } else { Some(Color::Rgb(55, 45, 15)) }
                } else if selected {
                    Some(Color::Rgb(40, 90, 50))
                } else {
                    Some(Color::Rgb(20, 45, 25))
                }
            } else if selected {
                Some(Color::DarkGray)
            } else {
                None
            };
            match bg {
                Some(c) => item.style(Style::default().bg(c)),
                None => item,
            }
        })
        .collect();
    let list = List::new(items)
        .block(block)
        .highlight_style(Style::default().add_modifier(Modifier::BOLD))
        .highlight_symbol("> ");
    let mut state = ListState::default();
    state.select(Some(app.cursor));
    f.render_stateful_widget(list, area, &mut state);
}

fn render_diff(f: &mut Frame, app: &App, area: Rect) {
    let focused = matches!(app.focus, Focus::Diff);
    let border_style = if focused {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default().fg(Color::DarkGray)
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" Diff ")
        .border_style(border_style);

    let lines: Vec<Line> = app.diff_text.lines().map(diff_line).collect();
    let p = Paragraph::new(lines)
        .block(block)
        .wrap(Wrap { trim: false })
        .scroll((app.diff_scroll, 0));
    f.render_widget(p, area);
}

fn diff_line(l: &str) -> Line<'static> {
    let style = if l.starts_with("+++") || l.starts_with("---") {
        Style::default().fg(Color::White).add_modifier(Modifier::BOLD)
    } else if l.starts_with('+') {
        Style::default().fg(Color::Green)
    } else if l.starts_with('-') {
        Style::default().fg(Color::Red)
    } else if l.starts_with("@@") {
        Style::default().fg(Color::Cyan)
    } else if l.starts_with("diff ") || l.starts_with("index ") {
        Style::default().fg(Color::DarkGray)
    } else {
        Style::default()
    };
    Line::from(Span::styled(l.to_string(), style))
}

fn render_footer(f: &mut Frame, app: &App, area: Rect) {
    if let Some(pending) = &app.pending {
        let frames = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
        let elapsed = pending.started.elapsed();
        let idx = ((elapsed.as_millis() / 80) as usize) % frames.len();
        let line = Line::from(vec![
            Span::raw(" "),
            Span::styled(
                frames[idx],
                Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
            ),
            Span::raw(" "),
            Span::styled(pending.label.clone(), Style::default().fg(Color::White)),
            Span::styled(
                format!("  ({:.1}s)", elapsed.as_secs_f32()),
                Style::default().fg(Color::DarkGray),
            ),
        ]);
        f.render_widget(Paragraph::new(line), area);
        return;
    }
    let toast = match &app.status_line {
        Some((msg, ts)) if ts.elapsed() < Duration::from_secs(5) => Some(msg.clone()),
        _ => None,
    };
    if let Some(t) = toast {
        let p = Paragraph::new(Line::from(Span::styled(
            format!(" {}", t),
            Style::default().fg(Color::Yellow),
        )));
        f.render_widget(p, area);
        return;
    }
    let hints = match (&app.modal, app.tab) {
        (Modal::Commit(_), _) => " Enter/Ctrl-S commit  Ctrl-A amend  Esc cancel",
        (Modal::Push(_), _) => " y/Enter confirm  ↑↓ pick  Esc cancel",
        (Modal::Confirm(_), _) => " y confirm  n/Esc cancel",
        (Modal::None, Tab::Status) => match app.focus {
            Focus::Files => " 1/2/3 tab  ↑↓ nav  →/← expand/collapse  Tab diff  Space stage  a all  u unstage  d discard  c commit  P push  r refresh  q quit",
            Focus::Diff => " 1/2/3 tab  ↑↓ scroll  PgUp/PgDn  Home/End  ←/Esc back  r refresh  q quit",
        },
        (Modal::None, Tab::Stash) => match app.stash.focus {
            StashFocus::List => " 1/2/3 tab  ↑↓ nav  Home/End  Enter/→ details  a apply  p pop  d drop  r refresh  q quit",
            StashFocus::Details => " 1/2/3 tab  ↑↓ scroll  PgUp/PgDn  Home/End  ←/Esc back  q quit",
        },
        (Modal::None, Tab::Log) => match app.log.focus {
            LogFocus::List => " 1/2/3 tab  ↑↓ nav  Home/End  Enter/→ details  r refresh  q quit",
            LogFocus::Details => " 1/2/3 tab  ↑↓ scroll  PgUp/PgDn  Home/End  ←/Esc back  q quit",
        },
    };
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(hints, Style::default().fg(Color::DarkGray)))),
        area,
    );
}

fn centered_rect(percent_x: u16, percent_y: u16, r: Rect) -> Rect {
    let popup_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(r);
    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(popup_layout[1])[1]
}

fn render_commit_modal(f: &mut Frame, app: &App, area: Rect) {
    let Modal::Commit(ci) = &app.modal else { return };
    let rect = centered_rect(70, 25, area);
    f.render_widget(Clear, rect);

    let (git_n, lfs_n) = app.staged_counts();
    let amend_tag = if ci.amend { " [AMEND]" } else { "" };
    let title = format!(" Commit{}  — staged: {} git, {} LFS ", amend_tag, git_n, lfs_n);
    let block = Block::default()
        .borders(Borders::ALL)
        .title(title)
        .border_style(Style::default().fg(Color::Cyan));
    f.render_widget(block, rect);

    let inner = Rect {
        x: rect.x + 1,
        y: rect.y + 1,
        width: rect.width.saturating_sub(2),
        height: rect.height.saturating_sub(2),
    };
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Length(3)])
        .split(inner);

    let label = Line::from(Span::styled(
        "Subject",
        Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
    ));
    f.render_widget(Paragraph::new(label), rows[0]);
    let subj_block = Block::default().borders(Borders::ALL);
    let subj_text = format!("{}_", ci.subject);
    f.render_widget(
        Paragraph::new(subj_text)
            .style(Style::default().fg(Color::White))
            .block(subj_block),
        rows[1],
    );
}

fn render_push_modal(f: &mut Frame, d: &PushDialog, area: Rect) {
    let rect = centered_rect(60, 30, area);
    f.render_widget(Clear, rect);
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" Push ")
        .border_style(Style::default().fg(Color::Cyan));
    let inner = block.inner(rect);
    f.render_widget(block, rect);

    match d {
        PushDialog::Confirm { remote, branch } => {
            let lines = vec![
                Line::from(""),
                Line::from(format!("  Create branch '{}' on remote '{}'?", branch, remote)),
                Line::from(""),
                Line::from("  This runs: git push -u <remote> <branch>"),
                Line::from(""),
                Line::from(Span::styled("  [y] confirm   [n/Esc] cancel", Style::default().fg(Color::DarkGray))),
            ];
            f.render_widget(Paragraph::new(lines), inner);
        }
        PushDialog::Pick { remotes, cursor, branch } => {
            let mut lines = vec![
                Line::from(format!("  Pick remote for branch '{}':", branch)),
                Line::from(""),
            ];
            for (i, r) in remotes.iter().enumerate() {
                let marker = if i == *cursor { "> " } else { "  " };
                let style = if i == *cursor {
                    Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)
                } else {
                    Style::default()
                };
                lines.push(Line::from(Span::styled(format!("{}{}", marker, r), style)));
            }
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled("  [↑↓] navigate  [Enter] select  [Esc] cancel", Style::default().fg(Color::DarkGray))));
            f.render_widget(Paragraph::new(lines), inner);
        }
        PushDialog::Running => {
            f.render_widget(Paragraph::new("  Pushing…"), inner);
        }
    }
}

fn render_confirm_modal(f: &mut Frame, c: &crate::app::ConfirmDialog, area: Rect) {
    let rect = centered_rect(60, 40, area);
    f.render_widget(Clear, rect);
    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!(" {} ", c.title))
        .border_style(Style::default().fg(Color::Yellow));
    let inner = block.inner(rect);
    f.render_widget(block, rect);
    let p = Paragraph::new(c.message.clone()).wrap(Wrap { trim: false });
    f.render_widget(p, inner);
}
