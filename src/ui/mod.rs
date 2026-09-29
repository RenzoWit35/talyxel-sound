mod logo;
mod theme;

use ratatui::{
    Frame,
    buffer::Buffer,
    layout::{Alignment, Constraint, Flex, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Clear, List, ListItem, Paragraph, Wrap},
};

use crate::{
    app::{App, BROWSE, Focus, Overlay},
    event::View,
    spotify::{Repeat, format_ms},
    updater,
};

pub fn draw(f: &mut Frame, app: &mut App) {
    let area = f.area();
    f.render_widget(Block::new().style(theme::base()), area);

    let mut title_right = Line::default();
    if let Some(v) = &app.update_available {
        let text = if app.updating {
            format!(" updating to v{v}… ")
        } else {
            format!(" ⬆ v{v} available — press U ")
        };
        title_right = Line::from(Span::styled(text, Style::new().fg(theme::BADGE))).right_aligned();
    }
    let outer = Block::bordered()
        .border_type(BorderType::Plain)
        .border_style(theme::border(false))
        .title(
            Line::from(Span::styled(
                format!(" Talyxel Sound - TUI v{} ", updater::current_version()),
                theme::heading(),
            ))
            .centered(),
        )
        .title(title_right);
    let inner = outer.inner(area);
    f.render_widget(outer, area);

    let [main, status] = Layout::vertical([Constraint::Min(0), Constraint::Length(1)]).areas(inner);
    let side_w = (main.width * 3 / 10).clamp(28, 40);
    let [left, right] =
        Layout::horizontal([Constraint::Length(side_w), Constraint::Min(0)]).areas(main);

    draw_sidebar(f, app, left);

    let tracks_h = (right.height * 2 / 5).max(5);
    let [now_area, viz_area, list_area] = Layout::vertical([
        Constraint::Length(6),
        Constraint::Min(6),
        Constraint::Length(tracks_h),
    ])
    .areas(right);
    draw_now_playing(f, app, now_area);
    draw_player(f, app, viz_area);
    draw_tracks(f, app, list_area);
    draw_status(f, app, status);

    match &app.overlay {
        Overlay::None => {}
        Overlay::Help => draw_help(f, area),
        Overlay::Search { input } => draw_search(f, area, input),
        Overlay::Devices { .. } => draw_devices(f, app, area),
    }
}

fn draw_sidebar(f: &mut Frame, app: &mut App, area: Rect) {
    let focused = app.focus == Focus::Sidebar && matches!(app.overlay, Overlay::None);
    let block = Block::new()
        .borders(Borders::RIGHT)
        .border_style(theme::border(false));
    let inner = block.inner(area);
    f.render_widget(block, area);

    let show_logo = inner.height >= logo::HEIGHT + 10 && inner.width >= logo::WIDTH;
    let [logo_area, list_area] = Layout::vertical([
        Constraint::Length(if show_logo { logo::HEIGHT + 1 } else { 0 }),
        Constraint::Min(0),
    ])
    .areas(inner);

    if show_logo {
        let mut lines: Vec<Line> = logo::LOGO
            .iter()
            .map(|l| Line::from(Span::styled(*l, theme::accent())))
            .collect();
        lines.push(Line::from(Span::styled(logo::WORDMARK, theme::heading())));
        f.render_widget(
            Paragraph::new(lines).alignment(Alignment::Center),
            logo_area,
        );
    }

    let mut items: Vec<ListItem> = Vec::new();
    let selected = app.sidebar.selected();
    // Section headers are rendered as separate, non-selectable rows; map selection accordingly.
    items.push(ListItem::new(Line::from(Span::styled(
        "Browse",
        theme::heading(),
    ))));
    for name in BROWSE {
        items.push(ListItem::new(format!("  {name}")));
    }
    items.push(ListItem::new(""));
    let playlists_header = if app.playlists_loaded {
        format!("Playlists ({})", app.playlists.len())
    } else if app.playlists_error.is_some() {
        "Playlists (retrying…)".to_string()
    } else {
        "Playlists (loading…)".to_string()
    };
    items.push(ListItem::new(Line::from(Span::styled(
        playlists_header,
        theme::heading(),
    ))));
    for p in &app.playlists {
        items.push(ListItem::new(format!("  {}", p.name)));
    }

    // Convert logical selection to row index (skip headers / spacer).
    let row = selected.map(|i| if i < BROWSE.len() { i + 1 } else { i + 3 });
    let mut state = ratatui::widgets::ListState::default().with_offset(0);
    state.select(row);
    let list = List::new(items)
        .highlight_style(theme::selected(focused))
        .style(theme::base());
    f.render_stateful_widget(
        list,
        list_area.inner(ratatui::layout::Margin::new(1, 0)),
        &mut state,
    );
}

fn draw_now_playing(f: &mut Frame, app: &App, area: Rect) {
    let block = Block::new()
        .borders(Borders::BOTTOM)
        .border_style(theme::border(false));
    let inner = block.inner(area).inner(ratatui::layout::Margin::new(2, 0));
    f.render_widget(block, area);

    let now = &app.now;
    let mut lines = vec![Line::from(Span::styled("Now Playing", theme::heading()))];
    match &now.track {
        Some(t) => {
            lines.push(Line::from(vec![
                Span::styled(
                    t.artists.clone(),
                    Style::new().fg(theme::FG).add_modifier(Modifier::BOLD),
                ),
                Span::raw(" - "),
                Span::styled(t.name.clone(), Style::new().fg(theme::FG)),
            ]));
            let album = match &t.year {
                Some(y) if !y.is_empty() => format!("{} ({y})", t.album),
                _ => t.album.clone(),
            };
            lines.push(Line::from(Span::styled(album, theme::muted())));
        }
        None => {
            lines.push(Line::from(Span::styled("Nothing playing", theme::muted())));
            lines.push(Line::from(Span::styled(
                "Select a track and press Enter, or press d to pick a device",
                theme::muted(),
            )));
        }
    }
    let queue_pos = now
        .track
        .as_ref()
        .and_then(|t| app.tracks.iter().position(|x| x.uri == t.uri))
        .map(|i| format!("{}/{}", i + 1, app.tracks.len()))
        .unwrap_or_default();
    let device = now
        .device_name
        .as_deref()
        .map(|d| format!("on {d}"))
        .unwrap_or_default();
    let [left, right] = Layout::horizontal([
        Constraint::Min(0),
        Constraint::Length(device.chars().count() as u16),
    ])
    .areas(Rect {
        y: inner.y + 3,
        height: 1.min(inner.height.saturating_sub(3)),
        ..inner
    });
    f.render_widget(Paragraph::new(lines), inner);
    f.render_widget(Paragraph::new(Span::raw(queue_pos)), left);
    f.render_widget(
        Paragraph::new(Span::styled(device, theme::accent())).right_aligned(),
        right,
    );
}

fn draw_player(f: &mut Frame, app: &mut App, area: Rect) {
    let block = Block::new()
        .borders(Borders::BOTTOM)
        .border_style(theme::border(false));
    let inner = block.inner(area).inner(ratatui::layout::Margin::new(2, 0));
    f.render_widget(block, area);

    let [bars_area, progress_area, controls_area] = Layout::vertical([
        Constraint::Min(1),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .areas(inner);

    let n_bars = (bars_area.width as usize).div_ceil(2);
    app.spectrum_width = n_bars;
    render_spectrum(f.buffer_mut(), bars_area, &app.spectrum.bars);

    // Progress bar: [>=======--------] 2:54 / 5:59
    let now = &app.now;
    let (pos, dur) = (
        now.position(),
        now.track.as_ref().map_or(0, |t| t.duration_ms),
    );
    let times = format!(" {} / {}", format_ms(pos), format_ms(dur));
    let bar_w = (progress_area.width as usize).saturating_sub(times.len() + 3);
    let filled = if dur > 0 {
        (bar_w as u64 * pos as u64 / dur as u64) as usize
    } else {
        0
    };
    let progress = Line::from(vec![
        Span::styled("[", theme::accent()),
        Span::styled(">", theme::heading()),
        Span::styled("=".repeat(filled.min(bar_w)), theme::accent()),
        Span::styled(
            "-".repeat(bar_w - filled.min(bar_w)),
            Style::new().fg(theme::GREEN_DIM),
        ),
        Span::styled("]", theme::accent()),
        Span::raw(times),
    ]);
    f.render_widget(Paragraph::new(progress), progress_area);

    let state = if now.is_playing {
        "[Playing]"
    } else {
        "[Paused] "
    };
    let shuffle = if now.shuffle { "on" } else { "off" };
    let repeat = match now.repeat {
        Repeat::Off => "off",
        Repeat::Context => "all",
        Repeat::Track => "one",
    };
    let left = Line::from(vec![
        Span::styled(state, theme::heading()),
        Span::raw("   "),
        Span::styled("[<<]", theme::accent()),
        Span::raw("  "),
        Span::styled(
            if now.is_playing { "[||]" } else { "[> ]" },
            theme::accent(),
        ),
        Span::raw("  "),
        Span::styled("[>>]", theme::accent()),
        Span::raw("   "),
        Span::styled(
            format!("shuffle:{shuffle}  repeat:{repeat}"),
            theme::muted(),
        ),
    ]);
    let vol = format!("[Vol : {}%]", now.volume);
    let [l, r] = Layout::horizontal([Constraint::Min(0), Constraint::Length(vol.len() as u16)])
        .areas(controls_area);
    f.render_widget(Paragraph::new(left), l);
    f.render_widget(
        Paragraph::new(Span::styled(vol, theme::muted())).right_aligned(),
        r,
    );
}

/// Draws vertical bars one column wide with a one-column gap, using eighth blocks.
fn render_spectrum(buf: &mut Buffer, area: Rect, bars: &[f32]) {
    const EIGHTHS: [&str; 9] = [" ", "▁", "▂", "▃", "▄", "▅", "▆", "▇", "█"];
    let rows = area.height as usize;
    if rows == 0 {
        return;
    }
    for (i, &level) in bars.iter().enumerate() {
        let x = area.x + (i * 2) as u16;
        if x >= area.right() {
            break;
        }
        let total = (level.clamp(0.0, 1.0) * (rows * 8) as f32).round() as usize;
        let total = total.max(1);
        for row in 0..rows {
            let y = area.bottom() - 1 - row as u16;
            let fill = total.saturating_sub(row * 8).min(8);
            if fill == 0 {
                continue;
            }
            let color = if row as f32 / rows as f32 > 0.66 {
                theme::GREEN
            } else {
                theme::GREEN_DIM
            };
            let color = if row == 0 || fill == 8 {
                color
            } else {
                theme::GREEN
            };
            buf[(x, y)].set_symbol(EIGHTHS[fill]).set_fg(color);
        }
    }
}

fn draw_tracks(f: &mut Frame, app: &mut App, area: Rect) {
    let focused = app.focus == Focus::Tracks && matches!(app.overlay, Overlay::None);
    let rate_limited = app.api.rate_limited_for();
    let title = if app.loading && !rate_limited.is_zero() {
        format!(
            "{} (Spotify rate limit, retrying in {}s…)",
            app.view.title(),
            rate_limited.as_secs() + 1
        )
    } else if app.loading {
        format!("{} (loading…)", app.view.title())
    } else if app.view_error.is_some() {
        format!("{} (failed)", app.view.title())
    } else {
        format!("{} ({})", app.view.title(), app.tracks.len())
    };
    let block = Block::new().padding(ratatui::widgets::Padding::horizontal(2));
    let inner = block.inner(area);
    f.render_widget(block, area);
    let [title_area, list_area] =
        Layout::vertical([Constraint::Length(2), Constraint::Min(0)]).areas(inner);
    f.render_widget(
        Paragraph::new(Span::styled(
            title,
            if focused {
                theme::heading()
            } else {
                theme::base()
            },
        )),
        title_area,
    );

    if let Some(error) = app.view_error.as_ref().filter(|_| app.tracks.is_empty()) {
        let retry = match app.view {
            View::Search(_) => "Press / to search again.",
            _ => "Select it in the sidebar and press Enter to try again.",
        };
        let lines = vec![
            Line::from(Span::styled(
                format!("Couldn't load {}:", app.view.title()),
                Style::new().fg(theme::ERROR),
            )),
            Line::from(Span::raw(error.clone())),
            Line::default(),
            Line::from(Span::styled(retry, theme::muted())),
        ];
        f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), list_area);
        return;
    }

    let playing_uri = app.now.track.as_ref().map(|t| t.uri.as_str());
    let width = list_area.width as usize;
    let items: Vec<ListItem> = app
        .tracks
        .iter()
        .map(|t| {
            let is_playing = Some(t.uri.as_str()) == playing_uri;
            let marker = if is_playing { "♪ " } else { "  " };
            let dur = format_ms(t.duration_ms);
            let main = format!("{marker}{} — {}", t.name, t.artists);
            let pad = width.saturating_sub(main.chars().count() + dur.len() + 1);
            let text = if pad > 0 {
                format!("{main}{}{dur}", " ".repeat(pad))
            } else {
                truncate(&main, width.saturating_sub(dur.len() + 2)) + " " + &dur
            };
            let style = if is_playing {
                theme::accent()
            } else {
                theme::base()
            };
            ListItem::new(Span::styled(text, style))
        })
        .collect();
    let list = List::new(items).highlight_style(theme::selected(focused));
    f.render_stateful_widget(list, list_area, &mut app.track_state);
}

fn draw_status(f: &mut Frame, app: &App, area: Rect) {
    let line = match app.visible_status() {
        Some((msg, true)) => Line::from(Span::styled(
            format!(" {msg}"),
            Style::new().fg(theme::ERROR),
        )),
        Some((msg, false)) => Line::from(Span::styled(format!(" {msg}"), theme::accent())),
        None => Line::from(Span::styled(
            " Space play/pause · n/p next/prev · ←/→ seek · +/- vol · / search · d devices · ? help · q quit",
            theme::muted(),
        )),
    };
    f.render_widget(Paragraph::new(line), area);
}

fn popup(area: Rect, w: u16, h: u16) -> Rect {
    let [v] = Layout::vertical([Constraint::Length(h.min(area.height))])
        .flex(Flex::Center)
        .areas(area);
    let [r] = Layout::horizontal([Constraint::Length(w.min(area.width))])
        .flex(Flex::Center)
        .areas(v);
    r
}

fn popup_block(title: &str) -> Block<'_> {
    Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(theme::border(true))
        .title(Line::from(Span::styled(format!(" {title} "), theme::heading())).centered())
        .style(theme::base())
}

fn draw_help(f: &mut Frame, area: Rect) {
    let rows = [
        ("↑/↓  j/k", "move selection"),
        ("Tab  h/l", "switch pane"),
        ("Enter", "open / play"),
        ("Space", "play / pause"),
        ("n / p", "next / previous track"),
        ("← / →", "seek -5s / +5s"),
        ("+ / -", "volume up / down"),
        ("s / r", "shuffle / repeat (off → all → one)"),
        ("/", "search tracks"),
        ("d", "devices (Spotify Connect)"),
        ("U", "install available update"),
        ("q  Ctrl-C", "quit"),
    ];
    let lines: Vec<Line> = rows
        .iter()
        .map(|(k, d)| {
            Line::from(vec![
                Span::styled(format!("  {k:<11}"), theme::accent()),
                Span::raw(*d),
            ])
        })
        .collect();
    let r = popup(area, 54, rows.len() as u16 + 2);
    f.render_widget(Clear, r);
    f.render_widget(Paragraph::new(lines).block(popup_block("Keys")), r);
}

fn draw_search(f: &mut Frame, area: Rect, input: &str) {
    let r = popup(area, 60, 3);
    f.render_widget(Clear, r);
    let text = Line::from(vec![
        Span::raw(format!(" {input}")),
        Span::styled("█", theme::accent()),
    ]);
    f.render_widget(Paragraph::new(text).block(popup_block("Search Spotify")), r);
}

fn draw_devices(f: &mut Frame, app: &mut App, area: Rect) {
    let this_device: Vec<bool> = match &app.overlay {
        Overlay::Devices { devices, .. } => devices.iter().map(|d| app.is_this_device(d)).collect(),
        _ => return,
    };
    let Overlay::Devices {
        devices,
        state,
        loading,
        error,
    } = &mut app.overlay
    else {
        return;
    };

    // "▶ Name (this device)" on the left, "Kind  Vol%" on the right.
    let rows: Vec<(String, String)> = devices
        .iter()
        .zip(&this_device)
        .map(|(d, &this)| {
            let marker = if d.is_active { "▶ " } else { "  " };
            let suffix = if this { " (this device)" } else { "" };
            let volume = d.volume.map(|v| format!("{v:>3}%")).unwrap_or_default();
            let right = [d.kind.as_str(), volume.as_str()]
                .into_iter()
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>()
                .join("  ");
            (format!("{marker}{}{suffix}", d.name), right)
        })
        .collect();
    let widest = rows
        .iter()
        .map(|(l, r)| l.chars().count() + r.chars().count() + 4)
        .max()
        .unwrap_or(0) as u16;
    let width = (widest + 2).max(62).min(area.width);
    let height = (devices.len().max(3) as u16) + 2;
    let r = popup(area, width, height);
    f.render_widget(Clear, r);
    let title = if *loading && !devices.is_empty() {
        "Spotify Connect devices (refreshing…)"
    } else {
        "Spotify Connect devices"
    };
    let block = popup_block(title).title_bottom(
        Line::from(Span::styled(
            " ↑/↓ choose · Enter play there · r refresh · Esc close ",
            theme::muted(),
        ))
        .centered(),
    );

    if devices.is_empty() {
        let lines = match error {
            Some(e) => vec![
                Line::from(Span::styled(
                    " Couldn't load devices:",
                    Style::new().fg(theme::ERROR),
                )),
                Line::from(format!(" {e}")),
                Line::from(Span::styled(" Press r to try again.", theme::muted())),
            ],
            None if *loading => vec![Line::from(Span::styled(
                " Looking for devices…",
                theme::muted(),
            ))],
            None => vec![
                Line::from(" No devices found."),
                Line::from(Span::styled(
                    " Open Spotify on a phone, computer or speaker, then press r.",
                    theme::muted(),
                )),
            ],
        };
        f.render_widget(
            Paragraph::new(lines)
                .wrap(Wrap { trim: false })
                .block(block),
            r,
        );
        return;
    }

    let inner_width = r.width.saturating_sub(2) as usize;
    let items: Vec<ListItem> = rows
        .into_iter()
        .map(|(left, right)| {
            let pad = inner_width.saturating_sub(left.chars().count() + right.chars().count() + 1);
            ListItem::new(Line::from(vec![
                Span::raw(left),
                Span::raw(" ".repeat(pad)),
                Span::styled(right, theme::muted()),
            ]))
        })
        .collect();
    f.render_stateful_widget(
        List::new(items)
            .block(block)
            .highlight_style(theme::selected(true)),
        r,
        state,
    );
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
        out.push('…');
        out
    }
}

#[cfg(test)]
mod tests {
    use ratatui::{Terminal, backend::TestBackend};

    #[tokio::test]
    async fn renders_demo_layout() {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = crate::demo::app(tx);
        for _ in 0..20 {
            app.spectrum.idle(1.0, 40);
        }
        let mut terminal = Terminal::new(TestBackend::new(110, 34)).unwrap();
        terminal.draw(|f| super::draw(f, &mut app)).unwrap();
        terminal.draw(|f| super::draw(f, &mut app)).unwrap();
        let buf = terminal.backend().buffer();
        let text: String = (0..buf.area.height)
            .map(|y| {
                (0..buf.area.width)
                    .map(|x| buf[(x, y)].symbol())
                    .collect::<String>()
                    + "
"
            })
            .collect();
        println!("{text}");
        for needle in [
            "Talyxel Sound - TUI",
            "Now Playing",
            "Pink Floyd - Wish You Were Here",
            "Discover Weekly",
            "[Vol : 75%]",
            "available",
        ] {
            assert!(text.contains(needle), "missing {needle:?}");
        }
    }
    fn render(app: &mut crate::app::App) -> String {
        let mut terminal = Terminal::new(TestBackend::new(110, 34)).unwrap();
        terminal.draw(|f| super::draw(f, app)).unwrap();
        let buf = terminal.backend().buffer();
        (0..buf.area.height)
            .map(|y| {
                (0..buf.area.width)
                    .map(|x| buf[(x, y)].symbol())
                    .collect::<String>()
                    + "\n"
            })
            .collect()
    }

    fn demo() -> crate::app::App {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        crate::demo::app(tx)
    }

    fn devices_overlay(
        devices: Vec<crate::spotify::Device>,
        loading: bool,
        error: Option<&str>,
    ) -> crate::app::Overlay {
        crate::app::Overlay::Devices {
            devices,
            state: ratatui::widgets::ListState::default().with_selected(Some(0)),
            loading,
            error: error.map(str::to_string),
        }
    }

    #[tokio::test]
    async fn device_list_marks_this_device_and_explains_the_keys() {
        let mut app = demo();
        app.device_id = Some("1".into());
        app.overlay = devices_overlay(crate::demo::devices(), false, None);
        let text = render(&mut app);
        println!("{text}");
        for needle in [
            "Talyxel Sound (this device)",
            "Living Room Speaker",
            "Speaker",
            "55%",
            "Enter",
            "r refresh",
        ] {
            assert!(text.contains(needle), "missing {needle:?}");
        }
    }

    #[tokio::test]
    async fn device_list_says_why_it_is_empty() {
        let mut app = demo();
        app.overlay = devices_overlay(Vec::new(), false, Some("no connection"));
        let text = render(&mut app);
        assert!(text.contains("no connection"), "{text}");
        assert!(text.contains("r to try again"), "{text}");
        assert!(!text.contains("Looking for devices"), "{text}");
    }

    #[tokio::test]
    async fn track_pane_explains_a_failed_load() {
        let mut app = demo();
        app.view = crate::event::View::Liked;
        app.tracks.clear();
        app.view_error = Some("Spotify did not answer in time".into());
        let text = render(&mut app);
        assert!(text.contains("Couldn't load Liked Songs"), "{text}");
        assert!(text.contains("Spotify did not answer in time"), "{text}");
    }

    #[tokio::test]
    async fn sidebar_shows_the_playlist_loading_state() {
        let mut app = demo();
        app.playlists.clear();
        app.playlists_loaded = false;
        assert!(render(&mut app).contains("Playlists (loading…)"));
        app.playlists_error = Some("offline".into());
        assert!(render(&mut app).contains("Playlists (retrying…)"));
    }
}
