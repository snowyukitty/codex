//! Regression coverage for the caret jumping between UI regions while a frame paints.
//!
//! Reported in openai/codex#17823 (Tabby, Wave) and openai/codex#16687 (alacritty):
//! the caret visibly walks the status line, the composer and the footer while the
//! model streams, instead of staying where the frame put it.
//!
//! The terminals that show it are the ones that do not implement synchronized
//! output (DEC private mode 2026), so the `BeginSynchronizedUpdate` that opens a
//! frame buys nothing there and every piece of the frame is painted as it lands.
//! `vt100` implements no synchronized-output mode either, which makes it a faithful
//! stand-in: sampling it at each write boundary yields the caret states such a
//! terminal would actually show.

use std::io;
use std::io::LineWriter;
use std::io::Write;

use ratatui::backend::Backend;
use ratatui::backend::ClearType;
use ratatui::backend::WindowSize;
use ratatui::buffer::Cell;
use ratatui::layout::Position;
use ratatui::layout::Rect;
use ratatui::layout::Size;
use ratatui::prelude::CrosstermBackend;
use ratatui::style::Stylize;
use ratatui::text::Line;
use ratatui::widgets::Paragraph;

use crate::custom_terminal::Terminal as CustomTerminal;
use crate::tui::Tui;
use crate::tui::terminal_stderr::TerminalStderrGuard;

const WIDTH: u16 = 40;
const HEIGHT: u16 = 8;

/// What a viewer would see: whether the caret was drawn, and where.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct CaretState {
    visible: bool,
    position: (u16, u16),
}

/// A `vt100` screen that records the caret after every write it receives.
///
/// Production writes a frame through `Stdout`, which is line buffered, so a frame
/// reaches the terminal in several pieces rather than one. Keeping a `LineWriter`
/// in front of this recorder reproduces those boundaries instead of inventing
/// finer ones that no terminal would ever present.
struct CaretRecorder {
    parser: vt100::Parser,
    timeline: Vec<CaretState>,
}

impl CaretRecorder {
    fn new(width: u16, height: u16) -> Self {
        Self {
            parser: vt100::Parser::new(height, width, /*scrollback_len*/ 0),
            timeline: Vec::new(),
        }
    }

    fn sample(&mut self) {
        let screen = self.parser.screen();
        let state = CaretState {
            visible: !screen.hide_cursor(),
            position: screen.cursor_position(),
        };
        if self.timeline.last() != Some(&state) {
            self.timeline.push(state);
        }
    }
}

impl Write for CaretRecorder {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let written = self.parser.write(buf)?;
        self.sample();
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.parser.flush()
    }
}

/// A backend over the recorder that answers geometry from cached values, so the
/// test never queries the real terminal.
struct CaretTimelineBackend {
    inner: CrosstermBackend<LineWriter<CaretRecorder>>,
    size: Size,
}

impl CaretTimelineBackend {
    fn new(width: u16, height: u16) -> Self {
        crossterm::style::force_color_output(true);
        Self {
            inner: CrosstermBackend::new(LineWriter::new(CaretRecorder::new(width, height))),
            size: Size { width, height },
        }
    }

    /// Caret states in the order a terminal would have shown them.
    fn timeline(&mut self) -> Vec<CaretState> {
        // Anything still sitting in the line buffer has not been presented yet.
        let _ = self.inner.writer_mut().flush();
        self.inner.writer_mut().get_ref().timeline.clone()
    }
}

impl Write for CaretTimelineBackend {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.inner.writer_mut().write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.writer_mut().flush()
    }
}

impl Backend for CaretTimelineBackend {
    type Error = io::Error;

    fn draw<'a, I>(&mut self, content: I) -> io::Result<()>
    where
        I: Iterator<Item = (u16, u16, &'a Cell)>,
    {
        self.inner.draw(content)
    }

    fn hide_cursor(&mut self) -> io::Result<()> {
        self.inner.hide_cursor()
    }

    fn show_cursor(&mut self) -> io::Result<()> {
        self.inner.show_cursor()
    }

    fn get_cursor_position(&mut self) -> io::Result<Position> {
        Ok(Position { x: 0, y: 0 })
    }

    fn set_cursor_position<P: Into<Position>>(&mut self, position: P) -> io::Result<()> {
        self.inner.set_cursor_position(position)
    }

    fn clear(&mut self) -> io::Result<()> {
        self.inner.clear()
    }

    fn clear_region(&mut self, clear_type: ClearType) -> io::Result<()> {
        self.inner.clear_region(clear_type)
    }

    fn scroll_region_up(&mut self, region: std::ops::Range<u16>, amount: u16) -> io::Result<()> {
        self.inner.scroll_region_up(region, amount)
    }

    fn scroll_region_down(&mut self, region: std::ops::Range<u16>, amount: u16) -> io::Result<()> {
        self.inner.scroll_region_down(region, amount)
    }

    fn size(&self) -> io::Result<Size> {
        Ok(self.size)
    }

    fn window_size(&mut self) -> io::Result<WindowSize> {
        Ok(WindowSize {
            columns_rows: self.size,
            pixels: self.size,
        })
    }

    fn flush(&mut self) -> io::Result<()> {
        Write::flush(self)
    }
}

fn make_recording_tui() -> io::Result<Tui<CaretTimelineBackend>> {
    let terminal = CustomTerminal::with_screen_size_and_cursor_position_for_test(
        CaretTimelineBackend::new(WIDTH, HEIGHT),
        Size {
            width: WIDTH,
            height: HEIGHT,
        },
        Position { x: 0, y: 0 },
    );
    let stderr_guard = TerminalStderrGuard::install()?;
    Ok(Tui::new(
        terminal,
        /*enhanced_keys_supported*/ false,
        stderr_guard,
    ))
}

/// Distinct places the caret was drawn while it was visible.
fn visible_positions(timeline: &[CaretState]) -> Vec<(u16, u16)> {
    let mut seen: Vec<(u16, u16)> = Vec::new();
    for state in timeline {
        if state.visible && !seen.contains(&state.position) {
            seen.push(state.position);
        }
    }
    seen
}

#[tokio::test]
async fn a_streaming_frame_never_shows_the_caret_in_more_than_one_place() {
    let mut tui = make_recording_tui().expect("recording tui");
    tui.terminal.set_viewport_area(Rect::new(
        /*x*/ 0, /*y*/ 4, WIDTH, /*height*/ 4,
    ));

    // History insertion is what splits a frame across write boundaries: every line
    // ends in a newline and `Stdout` is line buffered, so the terminal is handed a
    // piece of the frame per row. This is the situation the reports describe -
    // output streaming in while the composer and status line repaint.
    tui.insert_history_lines(vec![
        Line::from("thinking about the request".dim()),
        Line::from("reading src/main.rs"),
        Line::from("reading src/lib.rs"),
        Line::from("writing the patch"),
    ]);

    tui.draw(/*height*/ 4, |frame| {
        let area = frame.area();
        frame.render_widget_ref(
            &Paragraph::new(vec![
                Line::from("Working (12s, esc to interrupt)".dim()),
                Line::from(""),
                Line::from("> tell me about this repo"),
                Line::from("gpt-5.1-codex-max high".dim()),
            ]),
            area,
        );
    })
    .expect("draw frame");

    let timeline = tui.terminal.backend_mut().timeline();
    let visible = visible_positions(&timeline);

    assert!(
        visible.len() <= 1,
        "the frame painted a visible caret in {} different places: {visible:?}\n\
         full timeline: {timeline:?}\n\
         On a terminal without synchronized output that is the caret jumping \
         between UI regions reported in openai/codex#17823.",
        visible.len()
    );
}
