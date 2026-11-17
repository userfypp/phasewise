//! Developer tooling: renders the real screens to SVG for the README. Run it
//! with `cargo test --all-features -- --ignored readme_screenshots`.

use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use ratatui::style::{Color, Modifier};

use super::app::Tab;
use super::data::{HomePhase, PlanState, PlanSummary};
use super::home::Mode;
use super::{home_view, view};
use crate::plan::AgentKind;

/// Colours of one theme (all `#rrggbb`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Theme {
    Dark,
    Light,
}

struct Palette {
    background: &'static str,
    default: &'static str,
    dim: &'static str,
    cyan: &'static str,
    green: &'static str,
    red: &'static str,
    yellow: &'static str,
}

impl Theme {
    fn palette(self) -> Palette {
        match self {
            Theme::Dark => Palette {
                background: "#1e1e2e",
                default: "#e4e4e7",
                dim: "#71717a",
                cyan: "#5ec4d6",
                green: "#7ccf8a",
                red: "#f07178",
                yellow: "#e5c07b",
            },
            Theme::Light => Palette {
                background: "#fbfbfa",
                default: "#27272a",
                dim: "#a1a1aa",
                cyan: "#0e7490",
                green: "#15803d",
                red: "#b91c1c",
                yellow: "#a16207",
            },
        }
    }
}

const CELL_WIDTH: f64 = 8.4;
const CELL_HEIGHT: f64 = 19.0;
const PADDING: f64 = 16.0;
/// Room above the first row for the window dots.
const BAR: f64 = 20.0;
const FONT: &str = "ui-monospace, SFMono-Regular, Menlo, Consolas, monospace";

fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// A run of cells with one style, starting at a column.
struct Run {
    column: usize,
    text: String,
    fill: &'static str,
    bold: bool,
    underline: bool,
}

fn run_tspan(run: &Run) -> Option<String> {
    let trimmed = run.text.trim_end();
    let leading = trimmed.len() - trimmed.trim_start().len();
    let text = trimmed.trim_start();
    if text.is_empty() {
        return None;
    }
    let x = (run.column + leading) as f64 * CELL_WIDTH;
    let mut attrs = format!("x=\"{x:.1}\" fill=\"{}\"", run.fill);
    if run.bold {
        attrs.push_str(" font-weight=\"600\"");
    }
    if run.underline {
        attrs.push_str(" text-decoration=\"underline\"");
    }
    Some(format!("<tspan {attrs}>{}</tspan>", escape(text)))
}

/// The buffer as an SVG: a terminal window, one `<text>` per row, one
/// `<tspan x=…>` per styled run, and one for every non-ASCII glyph so a
/// fallback font cannot shift the columns.
pub(super) fn buffer_to_svg(buffer: &Buffer, theme: Theme) -> String {
    let palette = theme.palette();
    let area = buffer.area;
    // Trailing empty rows are left out but one, so a short screen does not
    // leave a tall empty window.
    let used = (0..area.height)
        .rev()
        .find(|&row| {
            (0..area.width).any(|column| {
                !buffer[(area.x + column, area.y + row)]
                    .symbol()
                    .trim()
                    .is_empty()
            })
        })
        .map_or(0, |row| row + 1);
    let rows = (used + 1).min(area.height);
    let width = f64::from(area.width) * CELL_WIDTH + 2.0 * PADDING;
    let height = f64::from(rows) * CELL_HEIGHT + 2.0 * PADDING + BAR;
    let mut svg = format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{width:.1}\" height=\"{height:.1}\" viewBox=\"0 0 {width:.1} {height:.1}\" font-family=\"{FONT}\" font-size=\"14\">\n"
    );
    svg.push_str(&format!(
        "<rect width=\"{width:.1}\" height=\"{height:.1}\" rx=\"10\" fill=\"{}\"/>\n",
        palette.background
    ));
    for (i, dot) in ["#ff5f57", "#febc2e", "#28c840"].iter().enumerate() {
        svg.push_str(&format!(
            "<circle cx=\"{}\" cy=\"{}\" r=\"5\" fill=\"{dot}\"/>\n",
            PADDING + 6.0 + 18.0 * i as f64,
            PADDING
        ));
    }
    for row in 0..rows {
        let mut tspans: Vec<String> = Vec::new();
        let mut run: Option<Run> = None;
        let flush = |run: &mut Option<Run>, tspans: &mut Vec<String>| {
            if let Some(done) = run.take() {
                tspans.extend(run_tspan(&done));
            }
        };
        for column in 0..area.width {
            let cell = &buffer[(area.x + column, area.y + row)];
            let symbol = cell.symbol();
            let fill = match cell.fg {
                Color::DarkGray => palette.dim,
                Color::Cyan => palette.cyan,
                Color::Green => palette.green,
                Color::Red => palette.red,
                Color::Yellow => palette.yellow,
                _ => palette.default,
            };
            let bold = cell.modifier.contains(Modifier::BOLD);
            let underline = cell.modifier.contains(Modifier::UNDERLINED);
            let column = usize::from(column);
            if !symbol.is_ascii() {
                flush(&mut run, &mut tspans);
                let own = Run {
                    column,
                    text: symbol.to_string(),
                    fill,
                    bold,
                    underline,
                };
                tspans.extend(run_tspan(&own));
                continue;
            }
            match &mut run {
                Some(r) if r.fill == fill && r.bold == bold && r.underline == underline => {
                    r.text.push_str(symbol);
                }
                _ => {
                    flush(&mut run, &mut tspans);
                    run = Some(Run {
                        column,
                        text: symbol.to_string(),
                        fill,
                        bold,
                        underline,
                    });
                }
            }
        }
        flush(&mut run, &mut tspans);
        if tspans.is_empty() {
            continue;
        }
        let y = PADDING + BAR + (f64::from(row) + 1.0) * CELL_HEIGHT - 5.0;
        svg.push_str(&format!(
            "<text y=\"{y:.1}\" xml:space=\"preserve\" transform=\"translate({PADDING} 0)\">{}</text>\n",
            tspans.join("")
        ));
    }
    svg.push_str("</svg>\n");
    svg
}

fn four_phase_plan() -> PlanState {
    let phase = |number, title: &str, checks, model: &str| HomePhase {
        agent: None,
        number,
        title: title.into(),
        checks,
        model: Some(model.into()),
        effort: None,
    };
    PlanState::Valid(PlanSummary {
        title: Some("Add a CSV export".into()),
        agent: AgentKind::Claude,
        agent_open: false,
        model: Some("claude-sonnet-5-5".into()),
        effort: None,
        phases: vec![
            phase(1, "Define the export format", 1, "claude-sonnet-5-5"),
            phase(2, "Write the CSV writer", 2, "claude-sonnet-5-5"),
            phase(3, "Add the export command", 2, "claude-opus-5-5"),
            phase(4, "Document the new command", 0, "claude-haiku-5-5"),
        ],
    })
}

fn home_buffer(open_models: bool) -> Buffer {
    let mut home = home_view::tests::home(four_phase_plan());
    home.data.plan_uncommitted = true;
    if open_models {
        home.set_models(AgentKind::Claude, Ok(home_view::tests::catalog()));
        home.model = Some("claude-sonnet-5-5".into());
        home.mode = Mode::ModelPopup(3);
    }
    let mut terminal = Terminal::new(TestBackend::new(100, 30)).expect("terminal");
    terminal
        .draw(|f| home_view::draw(f, &home))
        .expect("draw home");
    terminal.backend().buffer().clone()
}

fn run_buffer() -> Buffer {
    let mut app = view::tests::app();
    view::tests::with_diff(&mut app);
    app.tab = Tab::Diff;
    view::tests::render(&mut app, 100, 30)
        .backend()
        .buffer()
        .clone()
}

#[test]
#[ignore = "writes assets/readme/*.svg; run it by hand"]
fn readme_screenshots() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/readme");
    std::fs::create_dir_all(&dir).expect("create assets/readme");
    let shots = [
        ("home", home_buffer(false)),
        ("run", run_buffer()),
        ("home-models", home_buffer(true)),
    ];
    for (name, buffer) in &shots {
        for (suffix, theme) in [("dark", Theme::Dark), ("light", Theme::Light)] {
            let path = dir.join(format!("{name}-{suffix}.svg"));
            std::fs::write(&path, buffer_to_svg(buffer, theme)).expect("write svg");
        }
    }
}

#[cfg(test)]
mod tests {
    use ratatui::layout::Rect;
    use ratatui::style::Style;

    use super::*;

    #[test]
    fn a_tiny_buffer_becomes_positioned_styled_tspans() {
        let mut buffer = Buffer::empty(Rect::new(0, 0, 8, 2));
        buffer.set_string(0, 0, "ab ", Style::new());
        buffer.set_string(
            3,
            0,
            "❯x&",
            Style::new().fg(Color::Cyan).add_modifier(Modifier::BOLD),
        );
        buffer.set_string(
            0,
            1,
            "end",
            Style::new()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::UNDERLINED),
        );
        let dark = buffer_to_svg(&buffer, Theme::Dark);
        assert!(dark.contains("fill=\"#1e1e2e\""));
        assert_eq!(dark.matches("<circle").count(), 3);
        // The plain run, then the glyph on its own column, then the rest.
        assert!(
            dark.contains("<tspan x=\"0.0\" fill=\"#e4e4e7\">ab</tspan>"),
            "{dark}"
        );
        assert!(
            dark.contains("<tspan x=\"25.2\" fill=\"#5ec4d6\" font-weight=\"600\">❯</tspan>"),
            "{dark}"
        );
        assert!(
            dark.contains("<tspan x=\"33.6\" fill=\"#5ec4d6\" font-weight=\"600\">x&amp;</tspan>"),
            "{dark}"
        );
        assert!(
            dark.contains("fill=\"#71717a\" text-decoration=\"underline\">end</tspan>"),
            "{dark}"
        );
        assert_eq!(dark.matches("<text ").count(), 2);
        let light = buffer_to_svg(&buffer, Theme::Light);
        assert!(light.contains("fill=\"#fbfbfa\"") && light.contains("fill=\"#0e7490\""));
    }

    #[test]
    fn trailing_empty_rows_are_cut_but_one() {
        let mut buffer = Buffer::empty(Rect::new(0, 0, 4, 10));
        buffer[(1, 2)].set_symbol("x");
        let svg = buffer_to_svg(&buffer, Theme::Dark);
        let height = 4.0 * CELL_HEIGHT + 2.0 * PADDING + BAR;
        assert!(svg.contains(&format!("height=\"{height:.1}\"")), "{svg}");
    }

    #[test]
    fn rows_without_text_are_skipped() {
        let buffer = Buffer::empty(Rect::new(0, 0, 4, 3));
        assert_eq!(
            buffer_to_svg(&buffer, Theme::Dark)
                .matches("<text ")
                .count(),
            0
        );
    }
}
