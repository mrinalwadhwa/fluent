//! Display managed invocation terminals without owning worker execution.
mod client;
mod screen;
pub use client::EmbeddedTerminal;
pub use screen::{Cell, Frame, Screen};

pub struct Display {
    pub frame: Frame,
    pub identity: String,
    pub state: String,
}

impl EmbeddedTerminal {
    pub fn display(&self) -> Display {
        let d = self.descriptor();
        Display {
            frame: self.frame().clone(),
            identity: format!(
                "{} / {} / {} / {} / {}",
                d.provider, d.work_item_id, d.attempt_id, d.task_id, d.execution_generation
            ),
            state: self.diagnostic.clone().unwrap_or_else(|| "Observe".into()),
        }
    }
}

pub fn draw(frame: &mut ratatui::Frame, area: ratatui::layout::Rect, display: &Display) {
    use ratatui::{
        style::{Color, Modifier, Style},
        widgets::{Block, Borders, Paragraph},
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!(" Terminal · {} ", display.state));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    frame.render_widget(
        Paragraph::new(display.identity.as_str()),
        ratatui::layout::Rect {
            height: inner.height.min(1),
            ..inner
        },
    );
    let top = inner.y + 1;
    for y in 0..display.frame.rows.min(inner.height.saturating_sub(1)) {
        for x in 0..display.frame.columns.min(inner.width) {
            let cell =
                &display.frame.cells[y as usize * display.frame.columns as usize + x as usize];
            if cell.width == 0 || x + u16::from(cell.width) > inner.width {
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
                inner.x + x,
                top + y,
                cell.text(),
                cell.width as usize,
                style,
            );
        }
    }
    if let Some((x, y)) = display.frame.cursor {
        if x < inner.width && y < inner.height.saturating_sub(1) {
            frame.set_cursor_position((inner.x + x, top + y));
        }
    }
}
