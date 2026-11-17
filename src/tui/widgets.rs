//! Rendering pieces both screens share: styles, the footer with priority
//! dropping, messages, popups (one renderer), picker rows, text input, the
//! error box, the shortcuts list and the header card. Rendering only: the
//! state lives in `shared.rs`, `home.rs` and `app.rs`.

use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph, Wrap};

use super::data::PhaseView;
use super::shared::{FooterItem, Level, Notice, SettingsPanel, ShortcutGroups, item};
use crate::display::{EventLine, limit_parts, limits_clock};
use crate::settings::{self, REGISTRY};
use crate::state::RateLimits;
use crate::style::Role;

pub const MIN_WIDTH: u16 = 80;
pub const MIN_HEIGHT: u16 = 20;
/// Widest popup, in columns, unless it says otherwise.
pub const POPUP_WIDTH: u16 = 64;
/// Widest error popup.
pub const ERROR_WIDTH: u16 = 76;

/// ratatui style for a role; the same roles the CLI paints with ANSI codes.
/// The palette: Cyan (accent), DarkGray (dim), Green, Red, Yellow.
pub fn role_style(role: Role) -> Style {
    match role {
        Role::Default | Role::Tool => Style::new(),
        Role::Heading => Style::new().add_modifier(Modifier::BOLD),
        Role::Dim => dim(),
        Role::Ok => Style::new().fg(Color::Green),
        Role::Fail => Style::new().fg(Color::Red),
        Role::Warn => Style::new().fg(Color::Yellow),
        Role::Accent => accent(),
    }
}

/// Takes the colour out of a drawn frame, for `NO_COLOR`. Bold and underline
/// stay; the dim grey becomes the terminal's own dim attribute, so the
/// difference between a message and its detail does not depend on colour.
pub fn strip_color(buffer: &mut ratatui::buffer::Buffer) {
    for cell in &mut buffer.content {
        if cell.fg == Color::DarkGray {
            cell.modifier.insert(Modifier::DIM);
        }
        cell.fg = Color::Reset;
        cell.bg = Color::Reset;
    }
}

pub fn dim() -> Style {
    Style::new().fg(Color::DarkGray)
}

pub fn accent() -> Style {
    Style::new().fg(Color::Cyan)
}

pub fn bold() -> Style {
    Style::new().add_modifier(Modifier::BOLD)
}

pub fn accent_bold() -> Style {
    accent().add_modifier(Modifier::BOLD)
}

/// Number of terminal columns of a text (wide characters take two).
pub fn text_width(text: &str) -> usize {
    Span::raw(text).width()
}

pub fn char_width(c: char) -> usize {
    Span::raw(c.to_string()).width()
}

/// `text` split after at most `width` columns, never inside a wide
/// character; at least one character is taken so that callers progress.
fn split_at_width(text: &str, width: usize) -> (&str, &str) {
    let mut used = 0;
    for (i, c) in text.char_indices() {
        let w = char_width(c);
        if used + w > width && i > 0 {
            return text.split_at(i);
        }
        used += w;
    }
    (text, "")
}

/// `text` padded with spaces on the right to `width` columns.
pub fn pad_to(text: &str, width: usize) -> String {
    format!(
        "{text}{}",
        " ".repeat(width.saturating_sub(text_width(text)))
    )
}

pub fn line_width(line: &Line) -> usize {
    line.spans.iter().map(|s| text_width(&s.content)).sum()
}

/// `text` cut to at most `max` characters, ending with `…` when it was cut.
pub fn truncate(text: &str, max: usize) -> String {
    if text_width(text) <= max {
        return text.to_string();
    }
    let room = max.saturating_sub(1);
    let mut out = String::new();
    let mut used = 0;
    for c in text.chars() {
        let w = char_width(c);
        if used + w > room {
            break;
        }
        out.push(c);
        used += w;
    }
    if max > 0 {
        out.push('…');
    }
    out
}

/// `text` wrapped to `width` columns, by words; a word longer than the width
/// is split. Line breaks of the text are kept.
pub fn wrap_text(text: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut out = Vec::new();
    for paragraph in text.split('\n') {
        let mut current = String::new();
        for word in paragraph.split(' ').filter(|w| !w.is_empty()) {
            let used = text_width(&current);
            if used > 0 && used + 1 + text_width(word) <= width {
                current.push(' ');
                current.push_str(word);
                continue;
            }
            if used > 0 {
                out.push(std::mem::take(&mut current));
            }
            let mut rest = word;
            while text_width(rest) > width {
                let (head, tail) = split_at_width(rest, width);
                out.push(head.to_string());
                rest = tail;
            }
            current = rest.to_string();
        }
        out.push(current);
    }
    out
}

/// `45s`, `2m 05s` from a minute on, `1h 03m` from an hour on.
pub fn duration_text(secs: i64) -> String {
    crate::display::duration(u64::try_from(secs).unwrap_or(0))
}

/// Time elapsed since `at`, as `duration_text`.
pub fn elapsed_since(at: jiff::Timestamp) -> String {
    duration_text(jiff::Timestamp::now().as_second() - at.as_second())
}

/// The area with a one-column left margin.
pub fn margin(area: Rect) -> Rect {
    Rect {
        x: area.x.saturating_add(1),
        width: area.width.saturating_sub(1),
        ..area
    }
}

/// Message shown when the terminal is below the minimum size.
pub fn draw_too_small(frame: &mut Frame, area: Rect) {
    let text = format!(
        "phasewise needs a terminal of at least {MIN_WIDTH}×{MIN_HEIGHT} (now {}×{}). Enlarge the window.",
        area.width, area.height
    );
    let height = 3.min(area.height);
    let [_, middle, _] = Layout::vertical([
        Constraint::Fill(1),
        Constraint::Length(height),
        Constraint::Fill(1),
    ])
    .areas(area);
    frame.render_widget(
        Paragraph::new(text)
            .alignment(Alignment::Center)
            .wrap(Wrap { trim: true }),
        middle,
    );
}

/// Spans of an event line: the time column when the line has one (the Log tab
/// has none), the icon, the label and the detail.
pub fn event_spans(line: &EventLine) -> Vec<Span<'static>> {
    let style = role_style(line.role);
    let mut spans = Vec::new();
    if let Some(secs) = line.time {
        spans.push(Span::styled(
            format!("{:02}:{:02}  ", secs / 60, secs % 60),
            dim(),
        ));
    }
    spans.push(Span::styled(line.icon, style));
    if !line.label.is_empty() {
        let label = if line.detail.is_empty() {
            line.label.clone()
        } else {
            line.label_column()
        };
        spans.push(Span::styled(format!(" {label}"), style));
    }
    if !line.detail.is_empty() {
        spans.push(Span::styled(
            format!(" {}", line.detail),
            role_style(line.detail_role()),
        ));
    }
    spans
}

/// The text of an event line without icon or colour.
pub fn event_text(line: &EventLine) -> String {
    format!("{} {}", line.label, line.detail).trim().to_string()
}

/// Metadata joined by a dim ` · `.
pub fn dim_join(parts: &[String]) -> Span<'static> {
    Span::styled(parts.join(" · "), dim())
}

/// The limits line as spans, in `base` style; a window with a status other
/// than `allowed` is yellow. Parts that do not fit in `room` columns are left
/// out (the first is cut instead).
pub fn limit_spans(limits: &RateLimits, base: Style, room: usize) -> Vec<Span<'static>> {
    const GAP: &str = "   ";
    let (now, tz) = limits_clock();
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut used = 0;
    for part in limit_parts(limits, now, &tz) {
        let style = if part.warn {
            role_style(Role::Warn)
        } else {
            base
        };
        let gap = if spans.is_empty() { 0 } else { GAP.len() };
        if spans.is_empty() {
            let text = truncate(&part.text, room);
            used = text_width(&text);
            spans.push(Span::styled(text, style));
        } else if used + gap + text_width(&part.text) <= room {
            used += gap + text_width(&part.text);
            spans.push(Span::styled(GAP, base));
            spans.push(Span::styled(part.text, style));
        } else {
            break;
        }
    }
    spans
}

/// A message with its glyph, in the colour of its level.
pub fn notice_line(notice: &Notice) -> Line<'static> {
    let (glyph, role) = match notice.level {
        Level::Ok => ("✓ ", Role::Ok),
        Level::Warn => ("! ", Role::Warn),
        Level::Error => ("✗ ", Role::Fail),
        Level::Info => ("", Role::Default),
    };
    Line::styled(format!("{glyph}{}", notice.text), role_style(role))
}

/// The footer: `key action` pairs joined by a dim ` · `, keys in the default
/// colour, actions dim. Items are in priority order; the ones that do not fit
/// are dropped from the end, whole. `? shortcuts` (when `help`) always stays,
/// last.
pub fn footer_line(items: &[FooterItem], help: bool, width: usize) -> Line<'static> {
    let sep = 3;
    let width_of = |(key, action): &FooterItem| text_width(key) + 1 + text_width(action);
    let help_item = item("?", "shortcuts");
    let tail = if help { sep + width_of(&help_item) } else { 0 };
    let mut used = 0;
    let mut kept = 0;
    for (i, entry) in items.iter().enumerate() {
        let add = width_of(entry) + if i > 0 { sep } else { 0 };
        if used + add + tail > width {
            break;
        }
        used += add;
        kept += 1;
    }
    let mut shown: Vec<&FooterItem> = items.iter().take(kept).collect();
    if help {
        shown.push(&help_item);
    }
    let mut spans = Vec::new();
    for (i, (key, action)) in shown.into_iter().enumerate() {
        if i > 0 {
            spans.push(Span::styled(" · ", dim()));
        }
        spans.push(Span::raw(key.clone()));
        spans.push(Span::styled(format!(" {action}"), dim()));
    }
    Line::from(spans)
}

/// A popup: title, optional dim description, blank line, body. It is sized
/// to its content (no empty rows, no empty columns) within `max_width`.
pub struct Popup {
    pub title: Line<'static>,
    pub description: Option<String>,
    pub body: Vec<Line<'static>>,
    pub max_width: u16,
    /// First body line shown when the body does not fit the screen.
    pub scroll: usize,
}

impl Popup {
    pub fn new(title: &str) -> Popup {
        Popup {
            title: Line::styled(title.to_string(), bold()),
            description: None,
            body: Vec::new(),
            max_width: POPUP_WIDTH,
            scroll: 0,
        }
    }

    pub fn description(mut self, text: &str) -> Popup {
        self.description = Some(text.to_string());
        self
    }

    pub fn body(mut self, lines: Vec<Line<'static>>) -> Popup {
        self.body = lines;
        self
    }

    pub fn scroll(mut self, scroll: usize) -> Popup {
        self.scroll = scroll;
        self
    }

    /// Columns available to the text inside a popup on a screen `width` wide.
    pub fn text_room(&self, width: u16) -> usize {
        usize::from(self.max_width.min(width.saturating_sub(4)))
            .saturating_sub(4)
            .max(1)
    }
}

/// Draws the popup centred horizontally at a quarter of the height. Returns
/// the furthest the body can be scrolled (0 when it all fits).
pub fn render_popup(frame: &mut Frame, area: Rect, popup: &Popup) -> usize {
    let room = popup.text_room(area.width);
    let widest_body = popup.body.iter().map(line_width).max().unwrap_or(0);
    let description_width = popup.description.as_deref().map_or(0, text_width);
    let inner = line_width(&popup.title)
        .max(widest_body)
        .max(description_width)
        .min(room);
    let mut head = vec![popup.title.clone()];
    if let Some(description) = &popup.description {
        head.extend(
            wrap_text(description, inner)
                .into_iter()
                .map(|l| Line::styled(l, dim())),
        );
    }
    if !popup.body.is_empty() {
        head.push(Line::raw(""));
    }
    let height = u16::try_from(head.len() + popup.body.len() + 2)
        .unwrap_or(area.height)
        .min(area.height);
    // On a short screen the body scrolls under the title; its last row says
    // that there is more above or below.
    let rows = usize::from(height).saturating_sub(2 + head.len());
    let mut lines = head;
    let mut furthest = 0;
    if popup.body.len() > rows && rows >= 2 {
        let shown = rows - 1;
        furthest = popup.body.len() - shown;
        let offset = popup.scroll.min(furthest);
        lines.extend(popup.body[offset..offset + shown].iter().cloned());
        let more = match (offset > 0, offset + shown < popup.body.len()) {
            (true, true) => "↑↓ more",
            (true, false) => "↑ more",
            _ => "↓ more",
        };
        lines.push(Line::styled(more, dim()));
    } else {
        lines.extend(popup.body.iter().take(rows).cloned());
    }
    let width = u16::try_from(inner + 4)
        .unwrap_or(area.width)
        .min(area.width);
    let x = area.x + (area.width - width) / 2;
    let y = (area.y + area.height / 4).min(area.bottom().saturating_sub(height));
    let rect = Rect::new(x, y, width, height);
    frame.render_widget(Clear, rect);
    let block = Block::new()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(dim());
    let content = block.inner(rect);
    frame.render_widget(block, rect);
    let content = Rect {
        x: content.x + 1,
        width: content.width.saturating_sub(2),
        ..content
    };
    frame.render_widget(Paragraph::new(lines), content);
    furthest
}

/// A row of a picker.
pub enum PickerRow {
    Item {
        label: String,
        description: String,
        /// The current choice: a green `✔` at the end of the row.
        current: bool,
    },
    /// A dim line between items (loading, error); it takes no number.
    Note(Line<'static>),
}

impl PickerRow {
    pub fn item(label: &str, description: &str, current: bool) -> PickerRow {
        PickerRow::Item {
            label: label.to_string(),
            description: description.to_string(),
            current,
        }
    }
}

/// Picker lines: `❯ 1. Label   description   ✔`. `selected` counts items
/// only. Labels and descriptions are padded to their widest.
pub fn picker_lines(rows: &[PickerRow], selected: usize) -> Vec<Line<'static>> {
    let widest = |pick: fn(&PickerRow) -> Option<&str>| {
        rows.iter()
            .filter_map(pick)
            .map(text_width)
            .max()
            .unwrap_or(0)
    };
    let label_width = widest(|r| match r {
        PickerRow::Item { label, .. } => Some(label),
        PickerRow::Note(_) => None,
    });
    let description_width = widest(|r| match r {
        PickerRow::Item { description, .. } => Some(description),
        PickerRow::Note(_) => None,
    });
    let mut number = 0;
    let mut lines = Vec::new();
    for row in rows {
        match row {
            PickerRow::Note(line) => {
                let mut spans = vec![Span::raw("     ")];
                spans.extend(line.spans.iter().cloned());
                lines.push(Line::from(spans));
            }
            PickerRow::Item {
                label,
                description,
                current,
            } => {
                let is_selected = number == selected;
                let pointer = if is_selected {
                    Span::styled("❯ ", accent())
                } else {
                    Span::raw("  ")
                };
                let digit = if number < 9 {
                    format!("{}. ", number + 1)
                } else {
                    "   ".to_string()
                };
                let label_style = if is_selected {
                    accent_bold()
                } else {
                    Style::new()
                };
                let mut spans = vec![
                    pointer,
                    Span::raw(digit),
                    Span::styled(pad_to(label, label_width), label_style),
                ];
                if description_width > 0 {
                    spans.push(Span::styled(
                        format!("  {}", pad_to(description, description_width)),
                        dim(),
                    ));
                }
                if *current {
                    spans.push(Span::styled("  ✔", Style::new().fg(Color::Green)));
                }
                lines.push(Line::from(spans));
                number += 1;
            }
        }
    }
    lines
}

/// `> text█`, hard-wrapped at `width` columns.
pub fn input_lines(text: &str, width: usize) -> Vec<Line<'static>> {
    let room = width.saturating_sub(2).max(1);
    let mut chunks: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut used = 0;
    for c in text.chars() {
        let w = char_width(c);
        if used + w > room && !current.is_empty() {
            chunks.push(std::mem::take(&mut current));
            used = 0;
        }
        current.push(c);
        used += w;
    }
    chunks.push(current);
    // The cursor needs a column of its own.
    if chunks.last().is_some_and(|c| text_width(c) >= room) {
        chunks.push(String::new());
    }
    let last = chunks.len() - 1;
    chunks
        .into_iter()
        .enumerate()
        .map(|(i, chunk)| {
            let mut spans = vec![
                Span::raw(if i == 0 { "> " } else { "  " }),
                Span::raw(chunk),
            ];
            if i == last {
                spans.push(Span::styled("█", accent()));
            }
            Line::from(spans)
        })
        .collect()
}

/// Text for the content pane. The terminal buffer cannot hold control
/// characters (ratatui drops them, so a tab would swallow the indentation and
/// an escape sequence would leave its tail behind): a tab becomes 4 spaces,
/// any other control character (ESC, DEL, CR, C1) becomes `\u{FFFD}`.
pub fn sanitize(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '\t' => out.push_str("    "),
            c if c.is_control() => out.push('\u{FFFD}'),
            c => out.push(c),
        }
    }
    out
}

/// A text-input popup.
pub fn input_popup(title: &str, description: &str, text: &str, screen_width: u16) -> Popup {
    let popup = Popup::new(title).description(description);
    let width = popup.text_room(screen_width).min(60);
    let body = input_lines(text, width);
    // On a short screen the end of the text, where the cursor is, stays in
    // view (the popup clamps this to the end of the body).
    popup.body(body).scroll(usize::MAX)
}

/// A two-row confirmation: description (optional), then the action and the
/// way out, the highlighted row marked.
pub fn confirm_popup(
    title: &str,
    description: &str,
    action: &str,
    cancel: &str,
    row: usize,
) -> Popup {
    let rows = [
        PickerRow::item(action, "", false),
        PickerRow::item(cancel, "", false),
    ];
    let popup = Popup::new(title);
    let popup = if description.is_empty() {
        popup
    } else {
        popup.description(description)
    };
    popup.body(picker_lines(&rows, row))
}

/// An error box: the first line is the title (red `✗`), the rest is the
/// wrapped body.
pub fn error_popup(lines: &[String], screen_width: u16) -> Popup {
    let mut popup = Popup::new("");
    popup.max_width = ERROR_WIDTH;
    // The text of an error can come from a hook or a tool: ratatui would drop
    // its control characters and leave the rest of an escape sequence behind.
    let lines: Vec<String> = lines.iter().map(|l| sanitize(l)).collect();
    let title = lines.first().map_or("Error", String::as_str);
    popup.title = Line::styled(format!("✗ {title}"), role_style(Role::Fail));
    let room = popup.text_room(screen_width);
    let mut body = Vec::new();
    for line in lines.iter().skip(1) {
        let indent = text_width(line) - text_width(line.trim_start());
        let wrapped = wrap_text(line.trim_start(), room.saturating_sub(indent));
        for part in wrapped {
            body.push(Line::raw(format!("{}{part}", " ".repeat(indent))));
        }
    }
    // Drop a leading blank line: the popup adds its own.
    while body.first().is_some_and(|l| line_width(l) == 0) {
        body.remove(0);
    }
    popup.body(body)
}

/// The whole failure of a phase: what happened, wrapped, then where the logs
/// are (one path per line, never wrapped, so it can be copied).
pub fn failure_popup(phase: &PhaseView, screen_width: u16) -> Popup {
    let kind = phase.failure_kind.as_deref().unwrap_or("Failed");
    let mut popup = Popup::new("");
    popup.max_width = ERROR_WIDTH;
    popup.title = Line::styled(
        format!("✗ Phase {}: {kind}", phase.number),
        role_style(Role::Fail),
    );
    let room = popup.text_room(screen_width);
    let mut body = Vec::new();
    for line in phase.failure_detail.as_deref().unwrap_or("").trim().lines() {
        let line = sanitize(line);
        let line = line.as_str();
        let indent = text_width(line) - text_width(line.trim_start());
        for part in wrap_text(line.trim_start(), room.saturating_sub(indent)) {
            body.push(Line::raw(format!("{}{part}", " ".repeat(indent))));
        }
    }
    if !phase.logs.is_empty() {
        if !body.is_empty() {
            body.push(Line::raw(""));
        }
        body.push(Line::styled("Logs", dim()));
        body.extend(phase.logs.iter().map(|path| Line::raw(sanitize(path))));
    }
    popup.body(body)
}

/// The settings popup: label and value per row (changed values in the accent),
/// the selected row's description above, the text being typed in its row.
pub fn settings_popup(panel: &SettingsPanel, screen_width: u16) -> Popup {
    // The size comes from the registry only, never from the values, the
    // selection or the edit: the popup does not move while it is used.
    let label_width = REGISTRY
        .iter()
        .map(|s| text_width(s.label))
        .max()
        .unwrap_or(0);
    let value_width = REGISTRY
        .iter()
        .flat_map(|s| s.values().into_iter().chain([s.default, "(default)"]))
        .map(text_width)
        .max()
        .unwrap_or(0)
        .max(SETTINGS_VALUE_WIDTH);
    let row_width = 2 + label_width + 2 + value_width;
    let popup = Popup::new("Settings");
    let width = row_width
        .max(SETTINGS_MIN_WIDTH)
        .min(popup.text_room(screen_width));
    let description_lines = REGISTRY
        .iter()
        .map(|s| wrap_text(s.description, width).len())
        .max()
        .unwrap_or(1);
    // Fills a line with spaces to the popup's width.
    let padded = |mut line: Line<'static>| {
        let missing = width.saturating_sub(line_width(&line));
        line.spans.push(Span::raw(" ".repeat(missing)));
        line
    };
    let mut lines = Vec::new();
    for (i, spec) in REGISTRY.iter().enumerate() {
        let selected = i == panel.selected;
        let editing = selected.then_some(panel.editing.as_ref()).flatten();
        let value = match editing {
            // The end of the text is the part being typed.
            Some(text) => tail(&format!("{text}█"), value_width),
            None => truncate(
                &settings::shown(spec, &panel.settings.get(spec.key)),
                value_width,
            ),
        };
        let pointer = if selected {
            Span::styled("❯ ", accent())
        } else {
            Span::raw("  ")
        };
        let label_style = if selected {
            accent_bold()
        } else {
            Style::new()
        };
        let value_style = if editing.is_some() || panel.settings.is_changed(spec.key) {
            accent()
        } else {
            dim()
        };
        let gap = width.saturating_sub(2 + label_width + text_width(&value));
        lines.push(padded(Line::from(vec![
            pointer,
            Span::styled(pad_to(spec.label, label_width), label_style),
            Span::raw(" ".repeat(gap)),
            Span::styled(value, value_style),
        ])));
    }
    let description = REGISTRY
        .get(panel.selected)
        .map(|s| wrap_text(s.description, width))
        .unwrap_or_default();
    lines.push(padded(Line::raw("")));
    for i in 0..description_lines {
        let text = description.get(i).cloned().unwrap_or_default();
        lines.push(padded(Line::styled(text, dim())));
    }
    // Two lines are always reserved for the reason a typed value was refused.
    lines.push(padded(Line::raw("")));
    let why = panel
        .error
        .as_deref()
        .map(|why| wrap_text(why, width))
        .unwrap_or_default();
    for i in 0..SETTINGS_ERROR_LINES {
        let mut text = why.get(i).cloned().unwrap_or_default();
        if i + 1 == SETTINGS_ERROR_LINES && why.len() > SETTINGS_ERROR_LINES {
            text = truncate(&format!("{text} …"), width);
        }
        lines.push(padded(Line::styled(text, role_style(Role::Fail))));
    }
    popup.body(lines)
}

/// Narrowest content of the settings popup, in columns.
const SETTINGS_MIN_WIDTH: usize = 56;
/// Width of the value column of the settings popup, at least.
const SETTINGS_VALUE_WIDTH: usize = 24;
/// Lines reserved for an error in the settings popup.
const SETTINGS_ERROR_LINES: usize = 2;

/// The last `max` columns of `text`, with a leading `…` when it is cut.
fn tail(text: &str, max: usize) -> String {
    if text_width(text) <= max {
        return text.to_string();
    }
    let mut kept: Vec<char> = Vec::new();
    let mut used = 1;
    for c in text.chars().rev() {
        let w = char_width(c);
        if used + w > max {
            break;
        }
        used += w;
        kept.push(c);
    }
    kept.push('…');
    kept.reverse();
    kept.into_iter().collect()
}

/// The shortcuts popup: groups separated by a blank line, keys in the accent.
pub fn shortcuts_popup(groups: &ShortcutGroups) -> Popup {
    let width = groups
        .iter()
        .flat_map(|(_, rows)| rows.iter().map(|(key, _)| text_width(key)))
        .max()
        .unwrap_or(0);
    let mut lines = Vec::new();
    for (i, (group, rows)) in groups.iter().enumerate() {
        if i > 0 {
            lines.push(Line::raw(""));
        }
        lines.push(Line::styled((*group).to_string(), dim()));
        for (key, what) in rows {
            lines.push(Line::from(vec![
                Span::styled(format!("{key:<width$}"), accent()),
                Span::raw(format!("  {what}")),
            ]));
        }
    }
    Popup::new("Shortcuts").body(lines)
}

/// A rounded box sized to its lines (one column of padding), at the top-left
/// of `area`, at least `min_width` wide. Returns the height it took.
pub fn render_card(
    frame: &mut Frame,
    area: Rect,
    lines: Vec<Line<'static>>,
    min_width: u16,
) -> u16 {
    let widest = lines.iter().map(line_width).max().unwrap_or(0);
    let width = u16::try_from(widest + 4)
        .unwrap_or(area.width)
        .max(min_width)
        .min(area.width);
    let height = u16::try_from(lines.len() + 2)
        .unwrap_or(area.height)
        .min(area.height);
    let rect = Rect::new(area.x, area.y, width, height);
    let block = Block::new()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(dim());
    let inner = block.inner(rect);
    frame.render_widget(block, rect);
    let inner = Rect {
        x: inner.x + 1,
        width: inner.width.saturating_sub(2),
        ..inner
    };
    frame.render_widget(Paragraph::new(lines), inner);
    height
}

#[cfg(test)]
mod tests {
    #[test]
    fn strip_color_keeps_the_weight_and_turns_grey_into_dim() {
        use ratatui::buffer::Buffer;
        let area = Rect::new(0, 0, 3, 1);
        let mut buffer = Buffer::empty(area);
        buffer.set_string(0, 0, "a", Style::new().fg(Color::Cyan).bg(Color::Red));
        buffer.set_string(1, 0, "b", dim());
        buffer.set_string(2, 0, "c", accent_bold());
        strip_color(&mut buffer);
        let [a, b, c] = [0, 1, 2].map(|x| buffer[(x, 0)].clone());
        assert_eq!((a.fg, a.bg), (Color::Reset, Color::Reset));
        assert!(a.modifier.is_empty());
        assert_eq!(b.fg, Color::Reset);
        assert!(b.modifier.contains(Modifier::DIM));
        assert_eq!(c.fg, Color::Reset);
        assert!(c.modifier.contains(Modifier::BOLD));
    }

    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    use super::*;

    fn items(keys: &[(&str, &str)]) -> Vec<FooterItem> {
        keys.iter().map(|(k, a)| item(k, a)).collect()
    }

    fn text(line: &Line) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    #[test]
    fn footer_keeps_everything_when_it_fits() {
        let keys = items(&[("enter", "start"), ("p", "new plan"), ("q", "quit")]);
        assert_eq!(
            text(&footer_line(&keys, true, 80)),
            "enter start · p new plan · q quit · ? shortcuts"
        );
        assert_eq!(
            text(&footer_line(&keys, false, 80)),
            "enter start · p new plan · q quit"
        );
    }

    #[test]
    fn footer_drops_the_lowest_priority_items_whole_and_keeps_help() {
        let keys = items(&[("enter", "start"), ("p", "new plan"), ("q", "quit")]);
        // "enter start · p new plan · ? shortcuts" is 38 columns.
        assert_eq!(
            text(&footer_line(&keys, true, 38)),
            "enter start · p new plan · ? shortcuts"
        );
        assert_eq!(
            text(&footer_line(&keys, true, 37)),
            "enter start · ? shortcuts"
        );
        // Nothing fits besides the help: it stays alone, never cut.
        assert_eq!(text(&footer_line(&keys, true, 5)), "? shortcuts");
        assert_eq!(text(&footer_line(&keys, false, 3)), "");
    }

    #[test]
    fn wrap_text_breaks_by_words_and_splits_long_words() {
        assert_eq!(wrap_text("one two three", 7), ["one two", "three"]);
        assert_eq!(wrap_text("abcdefghij", 4), ["abcd", "efgh", "ij"]);
        assert_eq!(wrap_text("a\n\nb", 5), ["a", "", "b"]);
    }

    #[test]
    fn popups_have_no_empty_rows_or_columns() {
        let popup = confirm_popup(
            "Quit phasewise?",
            "The running phase is interrupted.",
            "Quit",
            "Keep working",
            0,
        );
        let mut terminal = Terminal::new(TestBackend::new(100, 30)).expect("terminal");
        terminal
            .draw(|f| {
                render_popup(f, f.area(), &popup);
            })
            .expect("draw");
        let buffer = terminal.backend().buffer().clone();
        let rows: Vec<String> = (0..30)
            .map(|y| {
                (0..100)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect();
        let used: Vec<&String> = rows.iter().filter(|r| r.trim() != "").collect();
        // Border, title, description, blank, two rows, border.
        assert_eq!(used.len(), 7, "{rows:#?}");
        assert!(used[0].contains("╭") && used[6].contains("╰"));
        // The widest content row plus border and padding is the box.
        let widest = used[1..6]
            .iter()
            .map(|r| r.trim().trim_matches('│').trim().chars().count())
            .max()
            .unwrap_or(0);
        assert_eq!(used[0].trim().chars().count(), widest + 4, "{used:#?}");
    }

    #[test]
    fn picker_rows_align_and_mark_the_current_choice() {
        let rows = [
            PickerRow::item("Default", "Claude's choice", true),
            PickerRow::item("Opus 5.5", "Complex work", false),
            PickerRow::Note(Line::styled("Loading…", dim())),
            PickerRow::item("Other…", "", false),
        ];
        let lines: Vec<String> = picker_lines(&rows, 1).iter().map(text).collect();
        assert_eq!(lines[0], "  1. Default   Claude's choice  ✔");
        assert_eq!(lines[1], "❯ 2. Opus 5.5  Complex work   ");
        assert_eq!(lines[2], "     Loading…");
        assert!(lines[3].starts_with("  3. Other…"));
    }

    #[test]
    fn input_wraps_and_shows_the_cursor() {
        let lines: Vec<String> = input_lines("abcdefgh", 6).iter().map(text).collect();
        assert_eq!(lines, ["> abcd", "  efgh", "  █"]);
        assert_eq!(text(&input_lines("", 10)[0]), "> █");
    }

    #[test]
    fn widths_are_terminal_columns() {
        assert_eq!(text_width("フェーズ"), 8);
        assert_eq!(text_width("a😀b"), 4);
        assert_eq!(truncate("フェーズ一二", 7), "フェー…");
        assert_eq!(truncate("フェーズ", 8), "フェーズ");
        assert_eq!(truncate("a😀b😀", 5), "a😀b…");
        assert_eq!(pad_to("フェ", 6), "フェ  ");
        assert_eq!(wrap_text("フェーズ フェーズ", 8), ["フェーズ", "フェーズ"]);
        assert_eq!(
            wrap_text("フェーズフェーズ", 6),
            ["フェー", "ズフェ", "ーズ"]
        );
        let lines: Vec<String> = input_lines("フェーズ", 6).iter().map(text).collect();
        assert_eq!(lines, ["> フェ", "  ーズ", "  █"]);
    }

    #[test]
    fn truncate_cuts_on_characters_and_marks_the_cut() {
        assert_eq!(truncate("short", 10), "short");
        assert_eq!(truncate("exactly10!", 10), "exactly10!");
        assert_eq!(truncate("0123456789ab", 10), "012345678…");
        assert_eq!(truncate("ñandú ñandú ñandú", 6), "ñandú…");
        assert_eq!(truncate("abc", 0), "");
        assert_eq!(truncate("abc", 1), "…");
    }
}
