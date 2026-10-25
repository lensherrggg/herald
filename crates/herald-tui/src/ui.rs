use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap};
use ratatui::Frame;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::app::{
    attachment_line, body_text, format_size, join_addrs, raw_rows, sender_short, shorten_date,
    wrap_line, App, Focus, Mode, RawRow,
};
use crate::browser::Browser;
use herald_core::model::{SearchField, SearchKind};

pub fn draw(frame: &mut Frame, app: &App) {
    let area = frame.area();
    if area.width < 40 || area.height < 8 {
        let message = Paragraph::new("终端太小，请放大到至少 40×8")
            .alignment(Alignment::Center)
            .wrap(Wrap { trim: true });
        frame.render_widget(message, area);
        return;
    }
    let footer_height = if area.height >= 12 { 2 } else { 1 };
    let layout = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(3),
        Constraint::Length(footer_height),
    ])
    .split(area);
    draw_header(frame, app, layout[0]);
    draw_main(frame, app, layout[1]);
    draw_footer(frame, app, layout[2]);

    match &app.mode {
        Mode::Browse => {}
        Mode::Input(state) => draw_input(frame, app, state),
        Mode::Accounts => draw_accounts(frame, app),
        Mode::Attachments => draw_attachments(frame, app),
        Mode::Browser => draw_browser(frame, app),
        Mode::Raw => draw_raw(frame, app),
        Mode::Help => draw_help(frame, app),
    }
}

fn draw_header(frame: &mut Frame, app: &App, area: Rect) {
    let account = app.account.as_deref().unwrap_or("(无账户)");
    let last_sync = app
        .sync_status
        .as_ref()
        .and_then(|status| status.last_sync_at.as_deref());
    let last_sync = if last_sync.is_some() {
        herald_core::time::china_short(last_sync)
    } else {
        "从未同步".to_string()
    };
    let sort_arrow = if app.descending { "↓" } else { "↑" };
    let title = format!(
        " Herald · {account} · 共 {} 封 · 未读 {} · 第 {}/{} 页 · 排序 {}{} · 同步 {last_sync} ",
        app.total,
        app.unread_total,
        app.page + 1,
        app.page_count(),
        app.sort.label(),
        sort_arrow
    );
    let line = Line::from(Span::styled(title, accent(app)));
    frame.render_widget(
        Paragraph::new(line).style(Style::new().add_modifier(Modifier::BOLD)),
        area,
    );
}

fn draw_main(frame: &mut Frame, app: &App, area: Rect) {
    if area.width >= 90 {
        let chunks = Layout::horizontal([Constraint::Percentage(45), Constraint::Percentage(55)])
            .split(area);
        draw_list(frame, app, chunks[0], app.focus == Focus::List);
        draw_detail(frame, app, chunks[1], app.focus == Focus::Detail);
    } else if app.focus == Focus::Detail && app.detail.is_some() {
        draw_detail(frame, app, area, true);
    } else {
        draw_list(frame, app, area, true);
    }
}

fn draw_list(frame: &mut Frame, app: &App, area: Rect, focused: bool) {
    let sort_arrow = if app.descending { "↓" } else { "↑" };
    let title = match &app.search {
        Some(search) => format!(
            "{} {}搜索（{}）：{} · {} 条 ",
            focus_marker(focused),
            search.label(),
            search.field.label(),
            search.query,
            app.total
        ),
        None => format!(
            "{} 邮件 · {}{} ",
            focus_marker(focused),
            app.sort.label(),
            sort_arrow
        ),
    };
    let block = panel(title, focused, app);
    if app.messages.is_empty() {
        let hint = if app.search.is_some() {
            "没有匹配的邮件\nEsc 清除搜索"
        } else {
            "还没有邮件\n按 s 同步 · / 搜索"
        };
        frame.render_widget(
            Paragraph::new(hint)
                .alignment(Alignment::Center)
                .block(block)
                .wrap(Wrap { trim: true }),
            area,
        );
        return;
    }
    let inner_width = area.width.saturating_sub(2) as usize;
    let items: Vec<ListItem> = app
        .messages
        .iter()
        .map(|message| {
            let marker = if message.is_read { "  " } else { "● " };
            let sender = truncate_line(
                &sender_short(message.from.as_deref()),
                inner_width.saturating_sub(2),
            );
            let top = Line::from(vec![
                Span::raw(marker),
                Span::styled(sender, Style::new().add_modifier(Modifier::BOLD)),
            ]);
            let subject = message
                .subject
                .clone()
                .filter(|subject| !subject.trim().is_empty())
                .unwrap_or_else(|| "(无主题)".to_string());
            let middle = Line::from(truncate_line(&subject, inner_width));
            let bottom = Line::from(Span::styled(
                shorten_date(message.date.as_deref()),
                subtle(app),
            ));
            ListItem::new(vec![top, middle, bottom])
        })
        .collect();
    let list = List::new(items).block(block).highlight_style(selected(app));
    let mut state = ListState::default().with_selected(Some(app.selected));
    frame.render_stateful_widget(list, area, &mut state);
}

/// Cut to a display-column budget, appending an ellipsis so a truncated
/// subject is visibly incomplete. Never splits a wide character in half.
fn truncate_line(text: &str, width: usize) -> String {
    if width == 0 {
        return String::new();
    }
    if UnicodeWidthStr::width(text) <= width {
        return text.to_string();
    }
    let budget = width - 1;
    let mut out = String::new();
    let mut used = 0;
    for ch in text.chars() {
        let ch_width = ch.width().unwrap_or(0);
        if used + ch_width > budget {
            break;
        }
        out.push(ch);
        used += ch_width;
    }
    let keep = out.trim_end().len();
    out.truncate(keep);
    out.push('…');
    out
}

fn subtle(app: &App) -> Style {
    if app.color {
        // DarkGray + DIM became unreadable on dark terminals; a normal gray
        // still separates the date from the subject while staying legible.
        Style::new().fg(Color::Gray)
    } else {
        Style::new().add_modifier(Modifier::DIM)
    }
}

fn draw_detail(frame: &mut Frame, app: &App, area: Rect, focused: bool) {
    let block = panel(format!("{} 详情 ", focus_marker(focused)), focused, app);
    let Some(detail) = &app.detail else {
        frame.render_widget(
            Paragraph::new("选择邮件查看详情\n\nr 查看原始邮件 · e 导出附件")
                .alignment(Alignment::Center)
                .block(block)
                .wrap(Wrap { trim: true }),
            area,
        );
        return;
    };

    let mut lines = vec![
        labelled(
            "主题",
            detail.header.subject.as_deref().unwrap_or("(无主题)"),
        ),
        labelled("发件人", detail.header.from.as_deref().unwrap_or("-")),
        labelled("收件人", &join_addrs(&detail.header.to)),
        labelled("抄送", &join_addrs(&detail.header.cc)),
        labelled(
            "时间（UTC+8）",
            &herald_core::time::china_datetime(detail.header.date.as_deref()),
        ),
        labelled("大小", &format_size(detail.header.size)),
        labelled("UIDL", &detail.header.uidl),
        labelled(
            "状态",
            if detail.is_read {
                "已读（本地）"
            } else {
                "未读（本地）"
            },
        ),
    ];
    if detail.attachments.is_empty() {
        lines.push(labelled("附件", "无"));
    } else {
        lines.push(labelled(
            "附件",
            &format!("{} 个（按 e 导出）", detail.attachments.len()),
        ));
        for meta in &detail.attachments {
            lines.push(Line::from(format!("  {}", attachment_line(meta))));
        }
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "── 正文 ──",
        Style::new().add_modifier(Modifier::DIM),
    )));
    let inner_width = area.width.saturating_sub(2).max(1);
    for line in body_text(detail).lines() {
        for piece in wrap_line(line, inner_width) {
            lines.push(Line::from(piece));
        }
    }
    let paragraph = Paragraph::new(Text::from(lines))
        .block(block)
        .wrap(Wrap { trim: false })
        .scroll((app.detail_scroll, 0));
    frame.render_widget(paragraph, area);
}

fn draw_footer(frame: &mut Frame, app: &App, area: Rect) {
    let status_style = if app.status.error {
        error_style(app)
    } else if app.color {
        Style::new().fg(Color::Green)
    } else {
        Style::new()
    };
    let status = Paragraph::new(Line::from(Span::styled(app.status_text(), status_style)));
    frame.render_widget(status, area);

    if area.height > 1 {
        let hints = match &app.mode {
            Mode::Browse if app.focus == Focus::Detail => {
                if area.width < 70 {
                    "↑↓ 滚动 · u 已读 · Tab 列表 · h 帮助"
                } else {
                    "↑↓ 滚动 · Tab 列表 · u 已读 · r 原文 · e 导出 · h 帮助"
                }
            }
            Mode::Browse => {
                if area.width < 70 {
                    "s 同步 · / 搜索 · o 排序 · u 已读 · Enter 详情"
                } else {
                    "s 同步 · / 搜索 · o 排序 · O 升降 · u 已读 · Enter 详情 · r 原文 · e 导出 · h 帮助"
                }
            }
            Mode::Input(_) => "Enter 确认 · Tab 模式 · Ctrl+F 字段 · Esc 取消",
            Mode::Accounts => "↑↓ 选择 · Enter 切换 · Esc 取消",
            Mode::Attachments => "↑↓ 选择附件 · Enter 导出 · Esc 取消",
            Mode::Browser => {
                "↑↓ 选择 · Enter 进入 · 退格 上级 · z 跳转 · / 筛选 · d 保存 · f 改名 · n 新建 · Esc 取消"
            }
            Mode::Raw => {
                let editing = app.raw_search.as_ref().is_some_and(|search| search.editing);
                let matched = app
                    .raw_search
                    .as_ref()
                    .is_some_and(|search| !search.matches.is_empty());
                if editing {
                    "输入关键字 · Enter 下一个 · Esc 完成"
                } else if matched {
                    "n/N 上下匹配 · / 重新查找 · Esc 返回"
                } else if area.width < 70 {
                    "/ 查找 · ↑↓ 行 · 空格/f 下页 · b 上页 · Esc 返回"
                } else {
                    "/ 查找 · ↑↓ 行 · 空格/f 下页 · b 上页 · d/u 半页 · g/G 首尾 · Esc 返回"
                }
            }
            Mode::Help => "Esc 返回",
        };
        let hint_area = Rect {
            y: area.y + 1,
            height: 1,
            ..area
        };
        frame.render_widget(
            Paragraph::new(Span::styled(
                hints,
                Style::new().add_modifier(Modifier::DIM),
            )),
            hint_area,
        );
    }
}

fn draw_input(frame: &mut Frame, app: &App, state: &crate::app::InputState) {
    let area = centered(frame.area(), 70, 25);
    if area.width < 10 || area.height < 3 {
        return;
    }
    frame.render_widget(Clear, area);
    let hint = match state.kind {
        crate::app::InputKind::Search => "Enter 确认 · Tab 模式 · Ctrl+F 字段 · Esc 取消",
        crate::app::InputKind::NewDir => "Enter 确认 · Esc 取消",
    };
    let title = match state.field_label() {
        Some(field) => format!(" {} · {} ", state.label(), field),
        None => format!(" {} ", state.label()),
    };
    let (before, after) = crate::text_input::split_at(&state.value, state.cursor);
    let text = Text::from(vec![
        Line::from(vec![
            Span::styled(before.to_string(), accent(app)),
            Span::styled("█", accent(app)),
            Span::styled(after.to_string(), accent(app)),
        ]),
        Line::from(Span::styled(hint, Style::new().add_modifier(Modifier::DIM))),
    ]);
    let paragraph = Paragraph::new(text)
        .block(panel(title, true, app))
        .wrap(Wrap { trim: false });
    frame.render_widget(paragraph, area);
}

fn draw_accounts(frame: &mut Frame, app: &App) {
    let height = (app.accounts.len() as u16 + 2).clamp(4, 16);
    let area = centered_fixed(frame.area(), 60, height);
    if area.width < 10 || area.height < 3 {
        return;
    }
    frame.render_widget(Clear, area);
    let items: Vec<ListItem> = app
        .accounts
        .iter()
        .map(|account| {
            let sync = account.last_sync_at.as_deref().unwrap_or("从未同步");
            ListItem::new(vec![
                Line::from(Span::styled(
                    format!("{}  ({}@{})", account.name, account.username, account.host),
                    Style::new().add_modifier(Modifier::BOLD),
                )),
                Line::from(format!("  最后同步 {sync} · {} 封", account.total_messages)),
            ])
        })
        .collect();
    let list = List::new(items)
        .block(panel(" 切换账户 ", true, app))
        .highlight_style(selected(app));
    let mut state = ListState::default().with_selected(Some(app.account_pick));
    frame.render_stateful_widget(list, area, &mut state);
}

fn draw_attachments(frame: &mut Frame, app: &App) {
    let Some(picker) = &app.attachments else {
        return;
    };
    let height = (picker.metas.len() as u16 * 2 + 2).clamp(4, 18);
    let area = centered_fixed(frame.area(), 80, height);
    if area.width < 10 || area.height < 3 {
        return;
    }
    frame.render_widget(Clear, area);
    let items: Vec<ListItem> = picker
        .metas
        .iter()
        .map(|meta| {
            ListItem::new(vec![
                Line::from(Span::styled(
                    format!(
                        "[{}] {}",
                        meta.index,
                        meta.filename.as_deref().unwrap_or("(未命名)")
                    ),
                    Style::new().add_modifier(Modifier::BOLD),
                )),
                Line::from(format!(
                    "    {} · {}{}",
                    meta.mime_type
                        .as_deref()
                        .unwrap_or("application/octet-stream"),
                    format_size(meta.size),
                    if meta.has_content {
                        " · 已缓存"
                    } else {
                        ""
                    }
                )),
            ])
        })
        .collect();
    let list = List::new(items)
        .block(panel(" 选择要导出的附件 ", true, app))
        .highlight_style(selected(app));
    let mut state = ListState::default().with_selected(Some(picker.selected));
    frame.render_stateful_widget(list, area, &mut state);
}

fn draw_browser(frame: &mut Frame, app: &App) {
    let Some(browser) = &app.browser else {
        return;
    };
    let area = centered(frame.area(), 90, 85);
    if area.width < 20 || area.height < 6 {
        return;
    }
    frame.render_widget(Clear, area);
    let block = panel(
        format!(" 导出附件 · 原名 {} ", browser.original_name),
        true,
        app,
    );
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let show_filter = browser.filtering || !browser.filter.is_empty();
    let mut heights = vec![Constraint::Min(1)];
    if show_filter {
        heights.push(Constraint::Length(1));
    }
    heights.push(Constraint::Length(1));
    heights.push(Constraint::Length(1));
    let chunks = Layout::vertical(heights).split(inner);
    let filename_chunk = if show_filter { 2 } else { 1 };
    let note_chunk = filename_chunk + 1;

    let items: Vec<ListItem> = browser
        .entries
        .iter()
        .map(|entry| {
            let label = Browser::entry_label(entry);
            let mut item = ListItem::new(label);
            if entry.is_dir {
                item = item.style(Style::new().add_modifier(Modifier::BOLD));
            }
            item
        })
        .collect();
    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(format!(" {} ", browser.dir.display())),
        )
        .highlight_style(selected(app));
    let mut state = ListState::default().with_selected(Some(browser.selected));
    frame.render_stateful_widget(list, chunks[0], &mut state);
    if show_filter {
        draw_browser_filter(frame, app, browser, chunks[1]);
    }

    let filename_style = if browser.filename_focus {
        accent(app)
    } else {
        Style::new()
    };
    let (before, after) = crate::text_input::split_at(&browser.filename, browser.filename_cursor);
    let mut filename_spans = vec![
        Span::styled(
            if browser.filename_focus {
                "文件名（编辑中）："
            } else {
                "文件名："
            },
            Style::new().add_modifier(Modifier::DIM),
        ),
        Span::styled(before.to_string(), filename_style),
    ];
    if browser.filename_focus {
        filename_spans.push(Span::styled("█", accent(app)));
    }
    filename_spans.push(Span::styled(after.to_string(), filename_style));
    let filename = Line::from(filename_spans);
    frame.render_widget(Paragraph::new(filename), chunks[filename_chunk]);

    let note = browser.note.clone().unwrap_or_else(|| {
        "Enter 进入/选文件 · 退格 上级 · z 跳转 · / 筛选 · d 存入当前目录 · f 改名".to_string()
    });
    frame.render_widget(
        Paragraph::new(Span::styled(
            note,
            if browser.note.is_some() {
                error_style(app)
            } else {
                Style::new().add_modifier(Modifier::DIM)
            },
        )),
        chunks[note_chunk],
    );
    draw_browser_jump(frame, app, browser);
}

fn draw_browser_jump(frame: &mut Frame, app: &App, browser: &Browser) {
    let Some(jump) = &browser.jump else {
        return;
    };
    let height = (jump.matches.len() as u16 + 3).clamp(5, 14);
    let area = centered_fixed(frame.area(), 76, height);
    if area.width < 20 || area.height < 4 {
        return;
    }
    frame.render_widget(Clear, area);
    let block = panel(
        " 跳转目录（z）· 输入名称片段 · Enter 跳转 · Esc 取消 ",
        true,
        app,
    );
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let chunks = Layout::vertical([Constraint::Length(1), Constraint::Min(1)]).split(inner);

    let (before, after) = crate::text_input::split_at(&jump.query, jump.cursor);
    let input = Line::from(vec![
        Span::styled("z ", Style::new().add_modifier(Modifier::DIM)),
        Span::raw(before.to_string()),
        Span::styled("█", accent(app)),
        Span::raw(after.to_string()),
    ]);
    frame.render_widget(Paragraph::new(input), chunks[0]);

    if jump.matches.is_empty() {
        let hint = if jump.query.trim().is_empty() {
            "还没有访问记录；从列表进入目录后即可跳转"
        } else {
            "没有匹配的目录；可输入完整绝对路径"
        };
        frame.render_widget(
            Paragraph::new(Span::styled(hint, Style::new().add_modifier(Modifier::DIM))),
            chunks[1],
        );
        return;
    }

    let items: Vec<ListItem> = jump
        .matches
        .iter()
        .map(|path| ListItem::new(path.display().to_string()))
        .collect();
    let list = List::new(items)
        .block(Block::default().borders(Borders::TOP))
        .highlight_style(selected(app));
    let mut state = ListState::default().with_selected(Some(jump.selected));
    frame.render_stateful_widget(list, chunks[1], &mut state);
}

fn draw_browser_filter(frame: &mut Frame, app: &App, browser: &Browser, area: Rect) {
    let (before, after) = crate::text_input::split_at(&browser.filter, browser.filter_cursor);
    let count = browser.filter_count();
    let hint = if !browser.filtering {
        "/ 编辑 · Esc 清除"
    } else if browser.filter_jump_target().is_some() {
        "Enter 跳转 · Esc 清除"
    } else {
        "Enter 确定 · Esc 清除"
    };
    let mut spans = vec![Span::styled(
        " 筛选: ",
        Style::new().add_modifier(Modifier::BOLD),
    )];
    if browser.filtering {
        spans.push(Span::raw(before.to_string()));
        spans.push(Span::styled("█", accent(app)));
        spans.push(Span::raw(after.to_string()));
    } else {
        spans.push(Span::raw(browser.filter.clone()));
    }
    spans.push(Span::styled(
        format!("  {count} 项  {hint}"),
        Style::new().add_modifier(Modifier::DIM),
    ));
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn draw_raw(frame: &mut Frame, app: &App) {
    let area = frame.area();
    let footer_height = if area.height >= 12 { 2 } else { 1 };
    let height = area.height.saturating_sub(footer_height);
    if height == 0 {
        return;
    }
    let raw_area = Rect { height, ..area };
    frame.render_widget(Clear, raw_area);
    let editing = app.raw_search.as_ref().is_some_and(|search| search.editing);
    let (content_area, input_area) = if editing && height >= 3 {
        let chunks = Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).split(raw_area);
        (chunks[0], Some(chunks[1]))
    } else {
        (raw_area, None)
    };
    let inner_width = content_area.width.saturating_sub(2).max(1);
    app.raw_width.set(inner_width);
    app.raw_height
        .set(content_area.height.saturating_sub(2).max(1));
    let rows = raw_rows(app.raw.as_deref().unwrap_or(""), inner_width);
    app.raw_rows_total.set(rows.len());
    let title = if app.raw_truncated {
        " 原始邮件（已截断）· Esc 返回 "
    } else {
        " 原始邮件 · Esc 返回 "
    };
    let paragraph = Paragraph::new(raw_text(app, &rows))
        .block(panel(title, true, app))
        .scroll((app.raw_scroll, 0));
    frame.render_widget(paragraph, content_area);
    if let Some(area) = input_area {
        draw_raw_search(frame, app, area);
    }
}

fn raw_text(app: &App, rows: &[RawRow]) -> Text<'static> {
    let raw = app.raw.as_deref().unwrap_or("");
    let source: Vec<&str> = raw.lines().collect();
    let search = app
        .raw_search
        .as_ref()
        .filter(|search| !search.matches.is_empty());
    let mut lines = Vec::with_capacity(rows.len());
    let mut cursor = 0usize;
    for row in rows {
        let content = source.get(row.line).copied().unwrap_or("");
        let Some(search) = search else {
            lines.push(Line::from(content[row.start..row.end].to_string()));
            continue;
        };
        while cursor < search.matches.len() && search.matches[cursor].line < row.line {
            cursor += 1;
        }
        let mut spans = Vec::new();
        let mut position = row.start;
        let mut scan = cursor;
        while scan < search.matches.len()
            && search.matches[scan].line == row.line
            && search.matches[scan].start < row.end
        {
            let matched = &search.matches[scan];
            if matched.end > row.start {
                let from = matched.start.max(row.start);
                let to = matched.end.min(row.end);
                if from > position {
                    spans.push(Span::raw(content[position..from].to_string()));
                }
                spans.push(Span::styled(
                    content[from..to].to_string(),
                    raw_match_style(app, scan == search.current),
                ));
                position = to;
            }
            scan += 1;
        }
        if position < row.end {
            spans.push(Span::raw(content[position..row.end].to_string()));
        }
        lines.push(Line::from(spans));
        while cursor < search.matches.len()
            && (search.matches[cursor].line < row.line
                || (search.matches[cursor].line == row.line
                    && search.matches[cursor].end <= row.end))
        {
            cursor += 1;
        }
    }
    Text::from(lines)
}

fn raw_match_style(app: &App, current: bool) -> Style {
    if app.color {
        if current {
            Style::new()
                .bg(Color::Yellow)
                .fg(Color::Black)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::new().bg(Color::Blue).fg(Color::White)
        }
    } else if current {
        Style::new().add_modifier(Modifier::REVERSED | Modifier::BOLD)
    } else {
        Style::new().add_modifier(Modifier::UNDERLINED)
    }
}

fn draw_raw_search(frame: &mut Frame, app: &App, area: Rect) {
    let Some(search) = &app.raw_search else {
        return;
    };
    let count = if search.matches.is_empty() {
        "0/0".to_string()
    } else {
        format!("{}/{}", search.current + 1, search.matches.len())
    };
    let hint = if search.matches.is_empty() {
        "输入关键字 · Esc 完成"
    } else {
        "Enter 下一个 · Esc 完成"
    };
    let (before, after) = crate::text_input::split_at(&search.query, search.cursor);
    let line = Line::from(vec![
        Span::styled(" 查找: ", Style::new().add_modifier(Modifier::BOLD)),
        Span::raw(before.to_string()),
        Span::styled("█", accent(app)),
        Span::raw(after.to_string()),
        Span::styled(
            format!("  {count}  {hint}"),
            Style::new().add_modifier(Modifier::DIM),
        ),
    ]);
    let style = if app.color {
        Style::new().bg(Color::DarkGray)
    } else {
        Style::new().add_modifier(Modifier::REVERSED)
    };
    frame.render_widget(Paragraph::new(line).style(style), area);
}

fn draw_help(frame: &mut Frame, app: &App) {
    let area = centered(frame.area(), 90, 85);
    if area.width < 20 || area.height < 6 {
        return;
    }
    frame.render_widget(Clear, area);
    let mut lines = vec![
        Line::from(Span::styled(
            "Herald 键盘操作",
            Style::new().add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from("s / S        增量同步 / 同步全部新邮件"),
        Line::from("/            搜索（Tab 模式 · Ctrl+F 字段）"),
        Line::from("a            切换账户"),
        Line::from("o / O        切换排序字段 / 反转升降序"),
        Line::from("[ ] PgUp/PgDn 翻页"),
        Line::from("↑ ↓ j k      移动选择（详情聚焦时滚动正文）"),
        Line::from("Enter        加载邮件详情（并标记已读）"),
        Line::from("u            标记已读 / 未读（仅本地）"),
        Line::from("Tab          切换列表/详情焦点"),
        Line::from("r            查看原始邮件（/ 查找，n/N 跳转）"),
        Line::from("e            导出附件（内置文件浏览器）"),
        Line::from("  浏览器      ↑↓ 选择 · Enter 进入 · 退格 上级 · z 跳转 · / 筛选 · d 保存"),
        Line::from("Esc          清除搜索 / 返回"),
        Line::from("q / Ctrl+C   退出"),
        Line::from(""),
        Line::from(Span::styled(
            "运行路径",
            Style::new().add_modifier(Modifier::BOLD),
        )),
    ];
    for line in app.paths().describe() {
        lines.push(Line::from(line));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(format!(
        "语义检索：{} · 模型文件：{} · 默认检索：{}",
        if app.herald.semantic_enabled() {
            "启用"
        } else {
            "关闭"
        },
        if app.semantic_model_present() {
            "已就绪"
        } else {
            "缺失（herald models download）"
        },
        match app.herald.default_search_kind(SearchField::All) {
            SearchKind::Hybrid => "混合",
            _ => "关键词",
        }
    )));
    let paragraph = Paragraph::new(Text::from(lines))
        .block(panel(" 帮助 · Esc 返回 ", true, app))
        .wrap(Wrap { trim: false });
    frame.render_widget(paragraph, area);
}

fn panel<'a>(title: impl Into<String>, focused: bool, app: &App) -> Block<'a> {
    let block = Block::default().borders(Borders::ALL).title(title.into());
    if focused {
        block.border_style(accent(app))
    } else {
        block
    }
}

fn focus_marker(focused: bool) -> &'static str {
    if focused {
        "▶"
    } else {
        " "
    }
}

fn labelled(label: &str, value: &str) -> Line<'static> {
    Line::from(vec![
        Span::styled(
            format!("{label}: "),
            Style::new().add_modifier(Modifier::BOLD),
        ),
        Span::raw(value.to_string()),
    ])
}

fn accent(app: &App) -> Style {
    if app.color {
        Style::new().fg(Color::Cyan).add_modifier(Modifier::BOLD)
    } else {
        Style::new().add_modifier(Modifier::BOLD)
    }
}

fn selected(app: &App) -> Style {
    let style = Style::new().add_modifier(Modifier::REVERSED | Modifier::BOLD);
    if app.color {
        style.fg(Color::Black).bg(Color::Cyan)
    } else {
        style
    }
}

fn error_style(app: &App) -> Style {
    if app.color {
        Style::new().fg(Color::Red).add_modifier(Modifier::BOLD)
    } else {
        Style::new().add_modifier(Modifier::BOLD)
    }
}

fn centered(area: Rect, percent_x: u16, percent_y: u16) -> Rect {
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

fn centered_fixed(area: Rect, width: u16, height: u16) -> Rect {
    let width = width.min(area.width);
    let height = height.min(area.height);
    Rect {
        x: area.x + (area.width - width) / 2,
        y: area.y + (area.height - height) / 2,
        width,
        height,
    }
}

#[cfg(test)]
mod tests {
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    use super::*;

    fn render_text(width: u16, height: u16) -> String {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("herald.toml");
        std::fs::write(
            &config_path,
            "[[accounts]]\nname = \"work\"\nhost = \"127.0.0.1\"\nport = 110\nsecurity = \"plain\"\nusername = \"u\"\npassword = \"p\"\n",
        )
        .unwrap();
        let herald = herald_core::Herald::open(Some(&config_path)).unwrap();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        let app = App::new(herald, runtime, None);
        terminal.draw(|frame| draw(frame, &app)).unwrap();
        terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>()
    }

    #[test]
    fn truncates_subjects_on_display_columns() {
        assert_eq!(truncate_line("项目进度汇报", 20), "项目进度汇报");
        assert_eq!(truncate_line("项目进度汇报", 7), "项目进…");
        assert_eq!(truncate_line("abcdef", 5), "abcd…");
        assert_eq!(truncate_line("abcde", 5), "abcde");
        assert_eq!(truncate_line("中文abc", 5), "中文…");
        assert_eq!(truncate_line("abc", 0), "");
    }

    #[test]
    fn tiny_terminals_degrade_without_panicking() {
        let text = render_text(30, 6);
        let compact: String = text.chars().filter(|ch| !ch.is_whitespace()).collect();
        assert!(compact.contains("终端太小"), "{compact}");
    }

    #[test]
    fn renders_header_and_list_empty_state() {
        let text = render_text(120, 40);
        let compact: String = text.chars().filter(|ch| !ch.is_whitespace()).collect();
        assert!(compact.contains("Herald"), "{compact}");
        assert!(compact.contains("work"), "{compact}");
        assert!(compact.contains("还没有邮件"), "{compact}");
    }
}
