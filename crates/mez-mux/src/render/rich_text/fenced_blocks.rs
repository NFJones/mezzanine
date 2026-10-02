//! Complete code-block capture and specialized/generic/literal presentation.
//!
//! Product callbacks run before generic highlighting. Generic highlighting keeps
//! one stateful highlighter across the block, while literal fallback sanitizes
//! terminal controls. Every path retains the existing raw-source copy contract.

use super::*;

impl<'a> MarkdownRenderer<'a> {
    /// Captures one complete code block so fenced renderers can inspect it.
    pub(super) fn handle_code_block_event(&mut self, event: Event<'_>) {
        match event {
            Event::End(TagEnd::CodeBlock) => self.render_code_block(),
            Event::Text(text) => {
                if let Some(block) = self.code_block.as_mut() {
                    block.body.push_str(text.as_ref());
                }
            }
            Event::SoftBreak | Event::HardBreak => {
                if let Some(block) = self.code_block.as_mut() {
                    block.body.push('\n');
                }
            }
            _ => {}
        }
    }

    /// Renders a captured code block through specialized, generic, or literal paths.
    pub(super) fn render_code_block(&mut self) {
        let Some(block) = self.code_block.take() else {
            return;
        };
        if block.fenced {
            if let Some(renderer) = self.fenced_block_renderer.as_mut() {
                match renderer(FencedCodeBlock {
                    info: block.info.as_str(),
                    body: block.body.as_str(),
                }) {
                    FencedCodeBlockOutcome::Rendered(lines) => {
                        self.lines.extend(lines);
                        self.finish_code_block();
                        return;
                    }
                    FencedCodeBlockOutcome::PreserveLiteral => {
                        self.append_fenced_literal_code_block(&block);
                        self.finish_code_block();
                        return;
                    }
                    FencedCodeBlockOutcome::NotHandled => {}
                }
            }
            if let Some(palette) = self.syntax_palette {
                let theme = super::super::syntax_theme("markdown-fence", palette);
                if let Some(mut highlighter) =
                    super::super::syntax_highlighter_for_fence(&block.info, &theme)
                {
                    self.start_generic_fenced_code_block();
                    let raw_fence = Self::fenced_code_block_source(&block);
                    for (index, source_line) in block.body.split_terminator('\n').enumerate() {
                        let mut line = self.literal_code_line(source_line);
                        let display = line.display.clone();
                        super::super::append_syntax_spans(&mut line, 0, &display, &mut highlighter);
                        line.copy_text = Some(if index == 0 {
                            raw_fence.clone()
                        } else {
                            COPY_SKIP_LINE.to_string()
                        });
                        line.kind = RichTextLineKind::MarkdownCodeBlock;
                        self.lines.push(line);
                    }
                    self.finish_code_block();
                    return;
                }
            }
        }
        if block.fenced {
            self.append_fenced_literal_code_block(&block);
        } else {
            self.append_literal_code_block(block.body.as_str());
        }
        self.finish_code_block();
    }

    /// Inserts one presentation-only blank row after a rendered code block.
    fn finish_code_block(&mut self) {
        if self
            .lines
            .last()
            .is_some_and(|line| !line.display.trim().is_empty())
        {
            self.lines.push(markdown_blank_line());
        }
    }

    /// Emits a fenced block with literal source delimiters for raw-copy alignment.
    fn append_fenced_literal_code_block(&mut self, block: &MarkdownCodeBlockState) {
        self.start_generic_fenced_code_block();
        let raw_fence = Self::fenced_code_block_source(block);
        for (index, source_line) in block.body.split_terminator('\n').enumerate() {
            let mut line = self.literal_code_line(source_line);
            line.copy_text = Some(if index == 0 {
                raw_fence.clone()
            } else {
                COPY_SKIP_LINE.to_string()
            });
            line.kind = RichTextLineKind::MarkdownCodeBlock;
            self.lines.push(line);
        }
    }

    /// Rebuilds the authored fenced Markdown retained by generic code presentation.
    fn fenced_code_block_source(block: &MarkdownCodeBlockState) -> String {
        format!("```{}\n{}```", block.info, block.body)
    }

    /// Inserts one presentation-only blank row before a generic fenced block.
    fn start_generic_fenced_code_block(&mut self) {
        if self
            .lines
            .last()
            .is_some_and(|line| !line.display.trim().is_empty())
        {
            self.lines.push(markdown_blank_line());
        }
    }

    /// Emits literal code rows with the existing neutral code foreground.
    fn append_literal_code_block(&mut self, body: &str) {
        for source_line in body.split_terminator('\n') {
            self.lines.push(self.literal_code_line(source_line));
        }
    }

    /// Builds one sanitized literal code row.
    fn literal_code_line(&self, source_line: &str) -> RichTextLine {
        let display = sanitized_terminal_line(source_line);
        let width = terminal_text_width(display.as_str());
        RichTextLine {
            display,
            style_spans: (width > 0)
                .then_some(TerminalStyleSpan {
                    start: 0,
                    length: width,
                    rendition: GraphicRendition {
                        foreground: Some(self.inline_code_foreground),
                        background: None,
                        inverse: false,
                        ..GraphicRendition::default()
                    },
                })
                .into_iter()
                .collect(),
            copy_text: None,
            kind: RichTextLineKind::Normal,
        }
    }
}
