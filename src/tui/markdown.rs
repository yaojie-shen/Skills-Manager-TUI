//! Keep Markdown table wrapping inside cells instead of splitting their borders.

use ratatui::style::Style;
use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthStr;

pub fn render(body: &str, width: usize) -> Vec<Line<'static>> {
    let lines: Vec<_> = tui_markdown::from_str(body)
        .lines
        .into_iter()
        .map(|line| Line {
            spans: line
                .spans
                .into_iter()
                .map(|s| Span::styled(s.content.into_owned(), s.style))
                .collect(),
            ..line
        })
        .collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        if let Some((end, rows, style)) = table_at(&lines, i) {
            if lines[i].width() > width {
                out.extend(layout(&rows, width, style));
            } else {
                out.extend_from_slice(&lines[i..=end]);
            }
            i = end + 1;
        } else {
            out.push(lines[i].clone());
            i += 1;
        }
    }
    out
}

type Row = Vec<Vec<Span<'static>>>;

// tui-markdown emits table borders as separate spans. Checking the complete
// structure avoids interpreting box-drawing characters inside fenced code (or
// inside a cell) as a table. Its generated padding is also in separate spans.
fn table_at(lines: &[Line<'static>], start: usize) -> Option<(usize, Vec<Row>, Style)> {
    let first = lines.get(start)?;
    if !border(first, '┌', '┬', '┐') {
        return None;
    }
    let header = cells(lines.get(start + 1)?)?;
    if !border(lines.get(start + 2)?, '├', '┼', '┤') {
        return None;
    }
    let count = header.len();
    let mut rows = vec![header];
    for (index, line) in lines.iter().enumerate().skip(start + 3) {
        if border(line, '└', '┴', '┘') {
            return Some((index, rows, first.spans[0].style));
        }
        let row = cells(line)?;
        if row.len() != count {
            return None;
        }
        rows.push(row);
    }
    None
}

fn border(line: &Line<'_>, left: char, middle: char, right: char) -> bool {
    if line.spans.len() != 1 {
        return false;
    }
    let s = line.spans[0].content.as_ref();
    s.starts_with(left)
        && s.ends_with(right)
        && s.chars()
            .skip(1)
            .take(s.chars().count().saturating_sub(2))
            .all(|c| c == '─' || c == middle)
}

fn cells(line: &Line<'static>) -> Option<Row> {
    let spans = &line.spans;
    if spans.first()?.content != "│" || spans.last()?.content != "│" {
        return None;
    }
    let mut row = Vec::new();
    let mut i = 1;
    while i < spans.len() {
        // Opening padding, content spans, closing padding, then a border.
        if !spans[i].content.chars().all(|c| c == ' ') {
            return None;
        }
        let begin = i + 1;
        i = begin;
        while i < spans.len() {
            if spans[i].content == "│"
                && i > begin
                && spans[i - 1].content.chars().all(|c| c == ' ')
            {
                break;
            }
            i += 1;
        }
        if i == spans.len() {
            return None;
        }
        row.push(spans[begin..i - 1].to_vec());
        i += 1;
    }
    Some(row)
}

fn layout(rows: &[Row], width: usize, border_style: Style) -> Vec<Line<'static>> {
    if width == 0 {
        return Vec::new();
    }
    let n = rows[0].len();
    if n == 0 {
        return Vec::new();
    }
    // Two columns of content permit CJK graphemes. When even that cannot fit,
    // show each row's cells vertically rather than dropping trailing columns.
    if width < n * 5 + 1 {
        let mut out = Vec::new();
        for row in rows {
            for cell in row {
                out.extend(wrap(cell, width));
            }
            out.push(Line::default());
        }
        return out;
    }
    let mut widths: Vec<usize> = (0..n)
        .map(|col| {
            rows.iter()
                .map(|row| row[col].iter().map(Span::width).sum::<usize>())
                .max()
                .unwrap_or(0)
                .max(2)
        })
        .collect();
    let budget = width - (n * 3 + 1);
    while widths.iter().sum::<usize>() > budget {
        let index = widths
            .iter()
            .enumerate()
            .max_by_key(|(_, w)| **w)
            .unwrap()
            .0;
        widths[index] -= 1;
    }
    let mut out = vec![rule(&widths, '┌', '┬', '┐', border_style)];
    for (index, row) in rows.iter().enumerate() {
        let wrapped: Vec<_> = row
            .iter()
            .zip(&widths)
            .map(|(cell, w)| wrap(cell, *w))
            .collect();
        let height = wrapped.iter().map(Vec::len).max().unwrap_or(1);
        for y in 0..height {
            let mut spans = vec![Span::styled("│", border_style)];
            for (cell, w) in wrapped.iter().zip(&widths) {
                spans.push(Span::raw(" "));
                let used = if let Some(line) = cell.get(y) {
                    spans.extend(line.spans.clone());
                    line.width()
                } else {
                    0
                };
                spans.push(Span::raw(" ".repeat(w.saturating_sub(used) + 1)));
                spans.push(Span::styled("│", border_style));
            }
            out.push(Line::from(spans));
        }
        if index == 0 {
            out.push(rule(&widths, '├', '┼', '┤', border_style));
        }
    }
    out.push(rule(&widths, '└', '┴', '┘', border_style));
    out
}

fn rule(widths: &[usize], left: char, middle: char, right: char, style: Style) -> Line<'static> {
    let body = widths
        .iter()
        .map(|w| "─".repeat(w + 2))
        .collect::<Vec<_>>()
        .join(&middle.to_string());
    Line::from(Span::styled(format!("{left}{body}{right}"), style))
}

fn wrap(spans: &[Span<'static>], width: usize) -> Vec<Line<'static>> {
    let mut out = Vec::new();
    let mut line = Line::default();
    let mut used = 0;
    for span in spans {
        for g in span.styled_graphemes(Style::default()) {
            let w = UnicodeWidthStr::width(g.symbol);
            if used > 0 && used + w > width {
                out.push(std::mem::take(&mut line));
                used = 0;
            }
            if let Some(last) = line.spans.last_mut().filter(|s| s.style == g.style) {
                last.content.to_mut().push_str(g.symbol);
            } else {
                line.spans.push(Span::styled(g.symbol.to_string(), g.style));
            }
            used += w;
        }
    }
    if !line.spans.is_empty() || out.is_empty() {
        out.push(line);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::style::Modifier;

    #[test]
    fn wraps_cjk_and_long_links_inside_columns() {
        let md = "| 名称 | 说明 |\n|---|---|\n| **审批流程** | [接口说明](references/approval/really-long-documentation-path.md) |";
        let lines = render(md, 36);
        assert!(lines.iter().all(|l| l.width() <= 36));
        assert!(lines.iter().all(|l| l.width() == lines[0].width()));
        let text: String = lines.iter().map(ToString::to_string).collect();
        assert!(text.contains("审批流程"));
        let plain: String = text
            .chars()
            .filter(|c| !c.is_whitespace() && !"│─┌┬┐├┼┤└┴┘".contains(*c))
            .collect();
        assert!(plain.contains("references/approval/really-long-documentation-path.md"));
        assert!(
            lines.iter().flat_map(|l| &l.spans).any(
                |s| s.content.contains("审批") && s.style.add_modifier.contains(Modifier::BOLD)
            )
        );
    }

    #[test]
    fn narrow_tables_keep_all_cells_and_wide_characters() {
        let md = "| 中文标题 | Second | Third |\n|---|---|---|\n| 甲乙 | https://example.com/long | final-cell |";
        let lines = render(md, 8);
        assert!(lines.iter().all(|l| l.width() <= 8));
        let text: String = lines.iter().map(ToString::to_string).collect();
        assert!(text.contains("甲乙"));
        assert!(text.contains("https://example.com/long"));
        assert!(text.contains("final-cell"));
    }

    #[test]
    fn fitting_tables_and_grapheme_clusters_keep_their_content() {
        let md =
            "| Symbols | Value |\n|---|---|\n| 👩‍💻 é | a very long value that forces wrapping |";
        assert_eq!(render(md, 120), tui_markdown::from_str(md).lines);
        let lines = render(md, 24);
        assert!(lines.iter().all(|l| l.width() <= 24));
        let text: String = lines.iter().map(ToString::to_string).collect();
        assert!(text.contains("👩‍💻"));
        assert!(text.contains("é"));
    }

    #[test]
    fn ordinary_markdown_and_box_drawing_code_are_unchanged() {
        let md =
            "# Heading\n\n**bold** and `code`\n\n```\n┌────┐\n│ hi │\n├────┤\n│ xx │\n└────┘\n```";
        assert_eq!(render(md, 8), tui_markdown::from_str(md).lines);
    }
}
