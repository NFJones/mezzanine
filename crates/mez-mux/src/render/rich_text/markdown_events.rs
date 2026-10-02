//! CommonMark event dispatch over the single Markdown renderer state.
//!
//! Parser options and event ordering remain fixed. Captured code blocks and
//! tables divert events to their existing owners without introducing another
//! source mapping, style stack or presentation policy.

use super::*;

impl<'a> MarkdownRenderer<'a> {
    /// Renders markdown using CommonMark plus the common GitHub-style extensions.
    pub(super) fn render(
        markdown: &str,
        theme: &RichTextTheme,
        table_display_width: Option<usize>,
        fenced_block_renderer: Option<&'a mut FencedCodeBlockRenderer>,
    ) -> Vec<RichTextLine> {
        let mut options = Options::empty();
        options.insert(Options::ENABLE_TABLES);
        options.insert(Options::ENABLE_FOOTNOTES);
        options.insert(Options::ENABLE_STRIKETHROUGH);
        options.insert(Options::ENABLE_TASKLISTS);
        options.insert(Options::ENABLE_SMART_PUNCTUATION);
        options.insert(Options::ENABLE_HEADING_ATTRIBUTES);
        options.insert(Options::ENABLE_MATH);
        options.insert(Options::ENABLE_GFM);
        options.insert(Options::ENABLE_DEFINITION_LIST);
        options.insert(Options::ENABLE_SUPERSCRIPT);
        options.insert(Options::ENABLE_SUBSCRIPT);
        options.insert(Options::ENABLE_WIKILINKS);

        let mut renderer = Self::new(theme, table_display_width, fenced_block_renderer);
        for event in Parser::new_ext(markdown, options) {
            renderer.handle_event(event);
        }
        renderer.finish_current_line();
        renderer.trim_trailing_blank_lines();
        renderer.lines
    }

    /// Handles one parser event, delegating table internals to table capture.
    fn handle_event(&mut self, event: Event<'_>) {
        if self.code_block.is_some() {
            self.handle_code_block_event(event);
            return;
        }
        if self.table.is_some() {
            self.handle_table_event(event);
            return;
        }
        match event {
            Event::Start(tag) => self.handle_start_tag(tag),
            Event::End(tag) => self.handle_end_tag(tag),
            Event::Text(text) => self.append_text(text.as_ref()),
            Event::Code(code) => self.append_code(code.as_ref()),
            Event::InlineMath(math) => self.append_inline_math(math.as_ref()),
            Event::DisplayMath(math) => self.append_display_math(math.as_ref()),
            Event::Html(html) => self.append_text(html.as_ref()),
            Event::InlineHtml(html) => self.handle_inline_html(html.as_ref()),
            Event::FootnoteReference(label) => self.append_text(&format!("[^{label}]")),
            Event::SoftBreak | Event::HardBreak => self.finish_current_line(),
            Event::Rule => {
                self.start_block();
                self.append_thematic_break();
                self.finish_current_line();
            }
            Event::TaskListMarker(checked) => self.replace_current_task_marker(checked),
        }
    }

    /// Handles the start of one markdown tag.
    fn handle_start_tag(&mut self, tag: Tag<'_>) {
        match tag {
            Tag::Paragraph => {
                if !self.current_prefix_only {
                    self.start_block();
                }
            }
            Tag::Heading { level, .. } => {
                self.start_block();
                self.line_copy_prefix = Some(format!("{} ", "#".repeat(level as usize)));
                let foreground = self.heading_foreground;
                self.push_style(|style| {
                    style.foreground = Some(foreground);
                    style.background = None;
                    style.bold = true;
                    style.underline = true;
                });
            }
            Tag::BlockQuote(kind) => {
                self.start_block();
                self.quote_depth = self.quote_depth.saturating_add(1);
                if let Some(kind) = kind {
                    self.append_text(&format!("[{kind:?}] "));
                }
            }
            Tag::CodeBlock(kind) => {
                self.start_block();
                let (info, fenced) = match kind {
                    CodeBlockKind::Fenced(info) => (info.into_string(), true),
                    CodeBlockKind::Indented => (String::new(), false),
                };
                self.code_block = Some(MarkdownCodeBlockState {
                    info,
                    fenced,
                    body: String::new(),
                });
            }
            Tag::HtmlBlock => self.start_block(),
            Tag::List(start) => self.list_stack.push(MarkdownListState {
                next_number: start.unwrap_or(1),
                ordered: start.is_some(),
            }),
            Tag::Item => self.start_list_item(),
            Tag::FootnoteDefinition(label) => {
                self.start_block();
                self.append_text(&format!("[^{label}]: "));
            }
            Tag::DefinitionList => self.start_block(),
            Tag::DefinitionListTitle => {
                self.start_block();
                self.push_style(|style| {
                    style.bold = true;
                });
            }
            Tag::DefinitionListDefinition => {
                self.start_block();
                self.append_text(": ");
            }
            Tag::Table(alignments) => {
                self.start_block();
                self.table = Some(MarkdownTableState::new(
                    alignments,
                    self.table_display_width,
                    self.heading_foreground,
                    self.structural_foreground,
                    self.table_alternate_row_foreground,
                ));
            }
            Tag::TableHead | Tag::TableRow | Tag::TableCell => {}
            Tag::Emphasis => self.push_style(|style| {
                style.italic = true;
            }),
            Tag::Strong => self.push_style(|style| {
                style.bold = true;
            }),
            Tag::Strikethrough => self.push_style(|style| {
                style.strikethrough = true;
            }),
            Tag::Superscript => self.push_style(|style| {
                style.bold = true;
            }),
            Tag::Subscript => self.push_style(|style| {
                style.dim = true;
            }),
            Tag::Link { dest_url, .. } => {
                self.link_stack.push(dest_url.to_string());
                let link_style = self.markdown_link_rendition();
                self.push_style(|style| *style = link_style);
            }
            Tag::Image { dest_url, .. } => {
                self.image_stack.push(dest_url.to_string());
                self.append_text("image: ");
                self.push_style(|style| {
                    style.italic = true;
                    style.underline = true;
                });
            }
            Tag::MetadataBlock(_) => self.start_block(),
        }
    }

    /// Handles the end of one markdown tag.
    fn handle_end_tag(&mut self, tag: TagEnd) {
        match tag {
            TagEnd::Paragraph => self.finish_paragraph(),
            TagEnd::Heading(_) => {
                self.pop_style();
                self.finish_current_line();
            }
            TagEnd::BlockQuote(_) => {
                self.finish_current_line();
                self.quote_depth = self.quote_depth.saturating_sub(1);
            }
            TagEnd::CodeBlock => self.render_code_block(),
            TagEnd::HtmlBlock => self.finish_current_line(),
            TagEnd::List(_) => {
                self.finish_current_line();
                self.list_stack.pop();
            }
            TagEnd::Item => {
                self.finish_current_line();
                self.continuation_prefix = None;
            }
            TagEnd::FootnoteDefinition => self.finish_current_line(),
            TagEnd::DefinitionList => self.finish_current_line(),
            TagEnd::DefinitionListTitle => {
                self.pop_style();
                self.finish_current_line();
            }
            TagEnd::DefinitionListDefinition => self.finish_current_line(),
            TagEnd::Table => {}
            TagEnd::TableHead | TagEnd::TableRow | TagEnd::TableCell => {}
            TagEnd::Emphasis
            | TagEnd::Strong
            | TagEnd::Strikethrough
            | TagEnd::Superscript
            | TagEnd::Subscript => self.pop_style(),
            TagEnd::Link => {
                self.pop_style();
                if let Some(dest_url) = self.link_stack.pop()
                    && !dest_url.is_empty()
                {
                    self.append_dim_text(&format!(" ({dest_url})"));
                }
            }
            TagEnd::Image => {
                self.pop_style();
                if let Some(dest_url) = self.image_stack.pop()
                    && !dest_url.is_empty()
                {
                    self.append_dim_text(&format!(" ({dest_url})"));
                }
            }
            TagEnd::MetadataBlock(_) => self.finish_current_line(),
        }
    }

    /// Handles parser events while a table is being captured.
    fn handle_table_event(&mut self, event: Event<'_>) {
        let mut render_table = false;
        let link_foreground = self.link_foreground;
        let inline_code_foreground = self.inline_code_foreground;
        if let Some(table) = self.table.as_mut() {
            match event {
                Event::Start(Tag::Table(_)) => {}
                Event::End(TagEnd::Table) => render_table = true,
                Event::Start(Tag::TableHead) => table.in_head = true,
                Event::End(TagEnd::TableHead) => {
                    if !table.current_cell.is_empty() {
                        table.finish_cell();
                    }
                    if !table.current_row.is_empty() {
                        table.finish_row();
                    }
                    table.header_rows = table.rows.len();
                    table.in_head = false;
                }
                Event::Start(Tag::TableRow) => table.start_row(),
                Event::End(TagEnd::TableRow) => table.finish_row(),
                Event::Start(Tag::TableCell) => table.start_cell(),
                Event::End(TagEnd::TableCell) => table.finish_cell(),
                Event::Start(Tag::Emphasis) => table.push_style(|style| style.italic = true),
                Event::Start(Tag::Strong) => table.push_style(|style| style.bold = true),
                Event::Start(Tag::Strikethrough) => {
                    table.push_style(|style| style.strikethrough = true);
                }
                Event::Start(Tag::Superscript) => table.push_style(|style| style.bold = true),
                Event::Start(Tag::Subscript) => table.push_style(|style| style.dim = true),
                Event::Start(Tag::Link { .. }) => {
                    table.link_depth = table.link_depth.saturating_add(1);
                    table.push_style(|style| {
                        style.foreground = Some(link_foreground);
                        style.background = None;
                        style.inverse = false;
                        style.bold = true;
                        style.underline = true;
                    });
                }
                Event::End(
                    TagEnd::Emphasis
                    | TagEnd::Strong
                    | TagEnd::Strikethrough
                    | TagEnd::Superscript
                    | TagEnd::Subscript,
                ) => table.pop_style(),
                Event::End(TagEnd::Link) => {
                    table.pop_style();
                    table.link_depth = table.link_depth.saturating_sub(1);
                }
                Event::Code(text) => {
                    let mut style = table.active;
                    style.foreground = Some(if table.link_depth == 0 {
                        inline_code_foreground
                    } else {
                        link_foreground
                    });
                    style.background = None;
                    style.inverse = false;
                    table.append_cell_styled_text(text.as_ref(), style);
                }
                Event::Text(text)
                | Event::InlineMath(text)
                | Event::DisplayMath(text)
                | Event::Html(text)
                | Event::InlineHtml(text)
                | Event::FootnoteReference(text) => {
                    table.append_cell_styled_text(text.as_ref(), table.active);
                }
                Event::SoftBreak | Event::HardBreak => table.append_cell_text(" "),
                Event::Rule => table.append_cell_text("────────"),
                Event::TaskListMarker(checked) => {
                    table.append_cell_text(if checked { "[x] " } else { "[ ] " });
                }
                Event::Start(_) | Event::End(_) => {}
            }
        }
        if render_table && let Some(table) = self.table.take() {
            self.lines.extend(table.render_lines());
        }
    }
}
