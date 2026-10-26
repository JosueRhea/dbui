//! The markdown the assistant answers in, laid out as GPUI elements: headings,
//! emphasis, inline code, lists, quotes, tables, and fenced code.

use std::ops::Range;

use gpui::{
    div, prelude::*, px, AnyElement, FontStyle, FontWeight, HighlightStyle, SharedString,
    StrikethroughStyle, StyledText, UnderlineStyle,
};
use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};

use crate::theme::{metrics, Theme};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mark {
    Strong,
    Emphasis,
    Strike,
    Code,
    Link,
}

/// One run of text with the marks laid over parts of it.
#[derive(Debug, Default, Clone, PartialEq)]
struct Inline {
    text: String,
    marks: Vec<(Range<usize>, Mark)>,
}

#[derive(Debug, Clone, PartialEq)]
enum Block {
    Paragraph(Inline),
    Heading(u8, Inline),
    Quote(Inline),
    /// A list item's text. Items nested in items carry a deeper `depth`;
    /// a second paragraph in the same item has an empty `marker`.
    Item {
        depth: usize,
        marker: String,
        text: Inline,
    },
    Code(String),
    Table {
        head: Vec<Inline>,
        rows: Vec<Vec<Inline>>,
    },
    Rule,
}

#[derive(Default)]
struct Builder {
    blocks: Vec<Block>,
    inline: Option<Inline>,
    open_marks: Vec<(usize, Mark)>,
    heading: Option<u8>,
    quotes: usize,
    /// One counter per open list: `Some(next)` numbers it, `None` bullets it.
    lists: Vec<Option<u64>>,
    marker: Option<String>,
    code: Option<String>,
    table: Option<(Vec<Inline>, Vec<Vec<Inline>>)>,
    in_head: bool,
}

impl Builder {
    fn inline(&mut self) -> &mut Inline {
        self.inline.get_or_insert_with(Inline::default)
    }

    fn push_text(&mut self, text: &str) {
        self.inline().text.push_str(text);
    }

    fn open(&mut self, mark: Mark) {
        let at = self.inline().text.len();
        self.open_marks.push((at, mark));
    }

    fn close(&mut self, mark: Mark) {
        if let Some(index) = self.open_marks.iter().rposition(|(_, m)| *m == mark) {
            let (start, _) = self.open_marks.remove(index);
            let inline = self.inline();
            let end = inline.text.len();
            if start < end {
                inline.marks.push((start..end, mark));
            }
        }
    }

    /// Ends the running text and files it under whatever holds it.
    fn flush(&mut self) {
        self.open_marks.clear();
        let Some(mut text) = self.inline.take() else {
            return;
        };
        let trimmed = text.text.trim_end().len();
        text.text.truncate(trimmed);
        for (range, _) in &mut text.marks {
            range.end = range.end.min(trimmed);
        }
        text.marks.retain(|(range, _)| range.start < range.end);
        if text.text.trim().is_empty() && self.marker.is_none() {
            return;
        }
        let block = if let Some(level) = self.heading {
            Block::Heading(level, text)
        } else if !self.lists.is_empty() {
            Block::Item {
                depth: self.lists.len() - 1,
                marker: self.marker.take().unwrap_or_default(),
                text,
            }
        } else if self.quotes > 0 {
            Block::Quote(text)
        } else {
            Block::Paragraph(text)
        };
        self.blocks.push(block);
    }

    fn event(&mut self, event: Event) {
        if let Some(code) = &mut self.code {
            match event {
                Event::Text(text) => code.push_str(&text),
                Event::End(TagEnd::CodeBlock) => {
                    let code = self.code.take().unwrap_or_default();
                    self.blocks
                        .push(Block::Code(code.trim_end_matches('\n').to_string()));
                }
                _ => {}
            }
            return;
        }
        match event {
            Event::Start(tag) => match tag {
                Tag::Paragraph => {}
                Tag::Heading { level, .. } => {
                    self.flush();
                    self.heading = Some(level as u8);
                }
                Tag::BlockQuote(_) => {
                    self.flush();
                    self.quotes += 1;
                }
                Tag::CodeBlock(_) => {
                    self.flush();
                    self.code = Some(String::new());
                }
                Tag::List(start) => {
                    self.flush();
                    self.lists.push(start);
                }
                Tag::Item => {
                    self.flush();
                    let marker = match self.lists.last_mut() {
                        Some(Some(next)) => {
                            *next += 1;
                            format!("{}.", *next - 1)
                        }
                        _ => "•".to_string(),
                    };
                    self.marker = Some(marker);
                }
                Tag::Table(_) => {
                    self.flush();
                    self.table = Some((Vec::new(), Vec::new()));
                }
                Tag::TableHead => self.in_head = true,
                Tag::TableRow => {
                    if let Some((_, rows)) = &mut self.table {
                        rows.push(Vec::new());
                    }
                }
                Tag::Emphasis => self.open(Mark::Emphasis),
                Tag::Strong => self.open(Mark::Strong),
                Tag::Strikethrough => self.open(Mark::Strike),
                Tag::Link { .. } => self.open(Mark::Link),
                _ => {}
            },
            Event::End(end) => match end {
                TagEnd::Paragraph => {
                    if self.table.is_none() {
                        self.flush();
                    }
                }
                TagEnd::Heading(_) => {
                    self.flush();
                    self.heading = None;
                }
                TagEnd::BlockQuote(_) => {
                    self.flush();
                    self.quotes = self.quotes.saturating_sub(1);
                }
                TagEnd::List(_) => {
                    self.flush();
                    self.lists.pop();
                }
                TagEnd::Item => {
                    self.flush();
                    self.marker = None;
                }
                TagEnd::TableCell => {
                    self.open_marks.clear();
                    let cell = self.inline.take().unwrap_or_default();
                    if let Some((head, rows)) = &mut self.table {
                        if self.in_head {
                            head.push(cell);
                        } else if let Some(row) = rows.last_mut() {
                            row.push(cell);
                        }
                    }
                }
                TagEnd::TableHead => self.in_head = false,
                TagEnd::Table => {
                    if let Some((head, rows)) = self.table.take() {
                        self.blocks.push(Block::Table { head, rows });
                    }
                }
                TagEnd::Emphasis => self.close(Mark::Emphasis),
                TagEnd::Strong => self.close(Mark::Strong),
                TagEnd::Strikethrough => self.close(Mark::Strike),
                TagEnd::Link => self.close(Mark::Link),
                _ => {}
            },
            Event::Text(text) | Event::Html(text) | Event::InlineHtml(text) => {
                self.push_text(&text)
            }
            Event::Code(text) => {
                self.open(Mark::Code);
                self.push_text(&text);
                self.close(Mark::Code);
            }
            Event::SoftBreak => self.push_text(" "),
            Event::HardBreak => self.push_text("\n"),
            Event::Rule => {
                self.flush();
                self.blocks.push(Block::Rule);
            }
            Event::TaskListMarker(done) => self.push_text(if done { "[x] " } else { "[ ] " }),
            _ => {}
        }
    }
}

fn parse(source: &str) -> Vec<Block> {
    let options =
        Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS;
    let mut builder = Builder::default();
    for event in Parser::new_ext(source, options) {
        builder.event(event);
    }
    builder.flush();
    if let Some(code) = builder.code.take() {
        // A fence still streaming in: show what has arrived.
        builder
            .blocks
            .push(Block::Code(code.trim_end_matches('\n').to_string()));
    }
    builder.blocks
}

fn styled(inline: &Inline, theme: &Theme) -> StyledText {
    let highlights: Vec<_> = inline
        .marks
        .iter()
        .map(|(range, mark)| {
            let style = match mark {
                Mark::Strong => HighlightStyle {
                    font_weight: Some(FontWeight::BOLD),
                    ..Default::default()
                },
                Mark::Emphasis => HighlightStyle {
                    font_style: Some(FontStyle::Italic),
                    ..Default::default()
                },
                Mark::Strike => HighlightStyle {
                    strikethrough: Some(StrikethroughStyle {
                        thickness: px(1.),
                        color: Some(theme.text_muted.into()),
                    }),
                    ..Default::default()
                },
                Mark::Code => HighlightStyle {
                    color: Some(theme.value_text.into()),
                    background_color: Some(theme.background.into()),
                    ..Default::default()
                },
                Mark::Link => HighlightStyle {
                    color: Some(theme.accent.into()),
                    underline: Some(UnderlineStyle {
                        thickness: px(1.),
                        color: Some(theme.accent.into()),
                        wavy: false,
                    }),
                    ..Default::default()
                },
            };
            (range.clone(), style)
        })
        .collect();
    StyledText::new(SharedString::from(inline.text.clone())).with_highlights(highlights)
}

fn render_block(block: &Block, theme: &Theme) -> AnyElement {
    match block {
        Block::Paragraph(text) => div().child(styled(text, theme)).into_any_element(),
        Block::Heading(level, text) => div()
            .pt_1()
            .font_weight(FontWeight::BOLD)
            .when(*level <= 2, |this| {
                this.text_size(metrics::text_size() * 1.15)
            })
            .child(styled(text, theme))
            .into_any_element(),
        Block::Quote(text) => div()
            .pl_2()
            .border_l_2()
            .border_color(theme.border)
            .text_color(theme.text_muted)
            .child(styled(text, theme))
            .into_any_element(),
        Block::Item {
            depth,
            marker,
            text,
        } => div()
            .flex()
            .gap_1()
            .pl(px(*depth as f32 * 14.))
            .child(
                div()
                    .flex_none()
                    .min_w(px(14.))
                    .text_color(theme.text_muted)
                    .child(marker.clone()),
            )
            .child(div().flex_1().min_w_0().child(styled(text, theme)))
            .into_any_element(),
        Block::Code(code) => div()
            .p_2()
            .rounded_md()
            .bg(theme.background)
            .border_1()
            .border_color(theme.border)
            .font_family(metrics::MONO_FONT)
            .text_size(metrics::text_size_small())
            .child(code.clone())
            .into_any_element(),
        Block::Table { head, rows } => {
            let columns = head.len().max(rows.iter().map(Vec::len).max().unwrap_or(0));
            let row = |cells: &[Inline], header: bool| {
                div()
                    .flex()
                    .when(header, |this| {
                        this.bg(theme.elevated).font_weight(FontWeight::BOLD)
                    })
                    .children((0..columns).map(|column| {
                        let cell = div()
                            .flex_1()
                            .min_w_0()
                            .px_1()
                            .py(px(2.))
                            .when(column > 0, |this| {
                                this.border_l_1().border_color(theme.divider)
                            });
                        match cells.get(column) {
                            Some(text) => cell.child(styled(text, theme)),
                            None => cell,
                        }
                    }))
            };
            div()
                .flex()
                .flex_col()
                .rounded_md()
                .border_1()
                .border_color(theme.border)
                .text_size(metrics::text_size_small())
                .child(row(head, true))
                .children(
                    rows.iter()
                        .map(|cells| row(cells, false).border_t_1().border_color(theme.divider)),
                )
                .into_any_element()
        }
        Block::Rule => div().h(px(1.)).bg(theme.divider).into_any_element(),
    }
}

/// `source` as markdown.
pub(crate) fn render(source: &str, theme: &Theme) -> AnyElement {
    div()
        .flex()
        .flex_col()
        .gap_2()
        .text_color(theme.text)
        .children(parse(source).iter().map(|block| render_block(block, theme)))
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plain(text: &str) -> Inline {
        Inline {
            text: text.into(),
            marks: Vec::new(),
        }
    }

    #[test]
    fn inline_marks_cover_their_text() {
        let blocks = parse("Use **bold**, *it* and `code` [here](https://x.y).");
        let [Block::Paragraph(text)] = blocks.as_slice() else {
            panic!("{blocks:?}");
        };
        assert_eq!(text.text, "Use bold, it and code here.");
        let marked: Vec<_> = text
            .marks
            .iter()
            .map(|(range, mark)| (&text.text[range.clone()], *mark))
            .collect();
        assert_eq!(
            marked,
            [
                ("bold", Mark::Strong),
                ("it", Mark::Emphasis),
                ("code", Mark::Code),
                ("here", Mark::Link),
            ]
        );
    }

    #[test]
    fn lists_nest_and_number() {
        let blocks = parse("1. one\n2. two\n   - inner\n\n- dot");
        let items: Vec<_> = blocks
            .iter()
            .map(|block| match block {
                Block::Item {
                    depth,
                    marker,
                    text,
                } => (*depth, marker.as_str(), text.text.as_str()),
                other => panic!("{other:?}"),
            })
            .collect();
        assert_eq!(
            items,
            [
                (0, "1.", "one"),
                (0, "2.", "two"),
                (1, "•", "inner"),
                (0, "•", "dot")
            ]
        );
    }

    #[test]
    fn tables_keep_head_and_rows() {
        let blocks = parse("Users:\n\n| id | name |\n|---|---|\n| 1 | **Ana** |\n| 2 | Luis |");
        assert_eq!(blocks[0], Block::Paragraph(plain("Users:")));
        let Block::Table { head, rows } = &blocks[1] else {
            panic!("{blocks:?}");
        };
        assert_eq!(head, &[plain("id"), plain("name")]);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0][1].text, "Ana");
        assert_eq!(rows[0][1].marks, [(0..3, Mark::Strong)]);
    }

    #[test]
    fn code_headings_and_unfinished_fences() {
        let blocks = parse("## Result\n\n```sql\nselect 1;\n```\n\n```sql\nselect 2");
        assert_eq!(
            blocks,
            [
                Block::Heading(2, plain("Result")),
                Block::Code("select 1;".into()),
                Block::Code("select 2".into()),
            ]
        );
    }
}
