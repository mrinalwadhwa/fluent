//! Display managed invocation terminals without owning worker execution.
mod attachment;
mod client;
mod fullscreen;
mod input;
mod remote;
mod screen;
pub use attachment::EmbeddedTerminal;
pub(crate) use fullscreen::DisplayRestoreFailed;
pub use input::{Input, releases_focus};
pub use screen::{Cell, Frame, Screen};

pub struct Display {
    pub frame: Frame,
    pub identity: String,
    pub state: String,
    pub focused: bool,
}

impl EmbeddedTerminal {
    pub fn display(&self) -> Display {
        Display {
            frame: self.frame().clone(),
            identity: self.identity(),
            state: self.state_label(),
            focused: self.focused(),
        }
    }
}

pub(crate) fn identity(d: &crate::terminal_sessions::TerminalDescriptor) -> String {
    format!(
        "{} / {} / {} / {} / {}",
        d.provider, d.work_item_id, d.attempt_id, d.task_id, d.execution_generation
    )
}

/// Wrap identity fields without clipping long Work Item IDs or generations.
pub(crate) fn identity_lines(identity: &str, width: u16) -> Vec<String> {
    use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};
    let width = usize::from(width.max(1));
    let mut lines = Vec::new();
    let mut line = String::new();
    let mut used = 0;
    for word in identity.split_whitespace() {
        if !line.is_empty() {
            if used + 1 + word.width() > width {
                lines.push(std::mem::take(&mut line));
                used = 0;
            } else {
                line.push(' ');
                used += 1;
            }
        }
        for character in word.chars() {
            let columns = character.width().unwrap_or(0);
            if used + columns > width && !line.is_empty() {
                lines.push(std::mem::take(&mut line));
                used = 0;
            }
            line.push(character);
            used += columns;
        }
    }
    lines.push(line);
    lines
}

/// Reserve the same identity rows for drawing, resizing, and mouse routing.
pub(crate) fn content_area(area: ratatui::layout::Rect, identity: &str) -> ratatui::layout::Rect {
    let inner = ratatui::layout::Rect {
        y: area.y + 1,
        height: area.height.saturating_sub(1),
        ..area
    };
    let rows = identity_lines(identity, inner.width)
        .len()
        .min(usize::from(inner.height)) as u16;
    ratatui::layout::Rect::new(inner.x, inner.y + rows, inner.width, inner.height - rows)
}

pub fn draw(frame: &mut ratatui::Frame, area: ratatui::layout::Rect, display: &Display) {
    use ratatui::{
        style::{Color, Modifier, Style},
        widgets::Paragraph,
    };
    let inner = ratatui::layout::Rect {
        y: area.y + 1,
        height: area.height.saturating_sub(1),
        ..area
    };
    let content = content_area(area, &display.identity);
    frame.render_widget(
        Paragraph::new(display.state.as_str()).style(
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        ratatui::layout::Rect { height: 1, ..area },
    );
    frame.render_widget(
        Paragraph::new(identity_lines(&display.identity, inner.width).join("\n")),
        ratatui::layout::Rect {
            height: content.y - inner.y,
            ..inner
        },
    );
    for y in 0..display.frame.rows.min(content.height) {
        for x in 0..display.frame.columns.min(content.width) {
            let cell =
                &display.frame.cells[y as usize * display.frame.columns as usize + x as usize];
            if cell.width == 0 || x + u16::from(cell.width) > content.width {
                continue;
            }
            let [r, g, b] = cell.foreground;
            let [br, bg, bb] = cell.background;
            let mut style = Style::default()
                .fg(Color::Rgb(r, g, b))
                .bg(Color::Rgb(br, bg, bb));
            for (bit, modifier) in [
                Modifier::BOLD,
                Modifier::ITALIC,
                Modifier::DIM,
                Modifier::SLOW_BLINK,
                Modifier::REVERSED,
                Modifier::HIDDEN,
                Modifier::CROSSED_OUT,
                Modifier::UNDERLINED,
            ]
            .into_iter()
            .enumerate()
            {
                if cell.flags & (1 << bit) != 0 {
                    style = style.add_modifier(modifier);
                }
            }
            frame.buffer_mut().set_stringn(
                content.x + x,
                content.y + y,
                cell.text(),
                cell.width as usize,
                style,
            );
        }
    }
    if let Some((x, y)) = display.frame.cursor {
        if x < content.width && y < content.height {
            frame.set_cursor_position((content.x + x, content.y + y));
        }
    }
}
