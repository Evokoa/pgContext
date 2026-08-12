//! Bounded structural adapters for the token chunking kernel.

use super::{
    MAX_STRUCTURE_DEPTH, MAX_STRUCTURE_SEGMENT_BYTES, ParsedBlock, Result, RetainedOutputBudget,
    StructureKind, TokenChunkError,
};
use crate::MAX_CHUNKS_PER_DOCUMENT;

pub(super) fn plain_blocks(
    source: &str,
    checkpoint: &mut impl FnMut() -> bool,
) -> Result<Vec<ParsedBlock>> {
    let mut blocks = Vec::new();
    let mut retained = RetainedOutputBudget::default();
    for (start, end) in paragraph_ranges_with_checkpoint(source, checkpoint)? {
        retained.admit_record::<ParsedBlock>(end - start, &[])?;
        blocks.push(ParsedBlock {
            start_byte: start,
            end_byte: end,
            retrieval_text: source[start..end].to_owned(),
            kind: StructureKind::Paragraph,
            path: Vec::new(),
            retrieval_maps_source: true,
        });
    }
    Ok(blocks)
}

pub(super) fn markdown_blocks(
    source: &str,
    checkpoint: &mut impl FnMut() -> bool,
) -> Result<Vec<ParsedBlock>> {
    let mut blocks = Vec::new();
    let mut retained = RetainedOutputBudget::default();
    let mut headings = Vec::<String>::new();
    let mut fence = None::<MarkdownFence>;
    let mut body_start = 0usize;
    let mut cursor = 0usize;
    let mut next_checkpoint = 0usize;
    while cursor < source.len() {
        require_parser_checkpoint(cursor, &mut next_checkpoint, checkpoint)?;
        let mut line_end = cursor;
        while source.as_bytes().get(line_end) != Some(&b'\n') && line_end < source.len() {
            require_parser_checkpoint(line_end, &mut next_checkpoint, checkpoint)?;
            line_end += 1;
        }
        let content_end = line_end
            .checked_sub(usize::from(
                source.as_bytes().get(line_end.wrapping_sub(1)) == Some(&b'\r'),
            ))
            .ok_or(TokenChunkError::ArithmeticOverflow)?;
        let line = &source[cursor..content_end];
        let next_line = line_end
            .checked_add(usize::from(line_end < source.len()))
            .ok_or(TokenChunkError::ArithmeticOverflow)?;
        if let Some(active) = fence {
            if markdown_fence_closes(line, active) {
                push_markdown_fence(
                    &mut blocks,
                    source,
                    active.start_byte,
                    content_end,
                    &headings,
                    &mut retained,
                )?;
                fence = None;
                body_start = next_line;
            }
        } else if let Some(opener) = markdown_fence_opener(line, cursor) {
            push_markdown_body(
                &mut blocks,
                source,
                body_start,
                cursor,
                &headings,
                &mut retained,
            )?;
            fence = Some(opener);
        } else if let Some((level, title)) = markdown_heading(line) {
            push_markdown_body(
                &mut blocks,
                source,
                body_start,
                cursor,
                &headings,
                &mut retained,
            )?;
            headings.truncate(level.saturating_sub(1));
            headings.push(title.to_owned());
            validate_structure_path(&headings)?;
            retained.admit_record::<ParsedBlock>(title.len(), &headings)?;
            push_block(
                &mut blocks,
                ParsedBlock {
                    start_byte: cursor,
                    end_byte: content_end,
                    retrieval_text: title.to_owned(),
                    kind: StructureKind::Heading,
                    path: headings.clone(),
                    retrieval_maps_source: false,
                },
            )?;
            body_start = next_line;
        }
        cursor = next_line;
    }
    if let Some(active) = fence {
        push_markdown_fence(
            &mut blocks,
            source,
            active.start_byte,
            source.len(),
            &headings,
            &mut retained,
        )?;
    } else {
        push_markdown_body(
            &mut blocks,
            source,
            body_start,
            source.len(),
            &headings,
            &mut retained,
        )?;
    }
    Ok(blocks)
}

const MAX_MARKDOWN_FENCE_BYTES: usize = MAX_STRUCTURE_SEGMENT_BYTES;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct MarkdownFence {
    marker: u8,
    length: usize,
    start_byte: usize,
}

fn markdown_fence_opener(line: &str, start_byte: usize) -> Option<MarkdownFence> {
    let bytes = line.as_bytes();
    let indentation = bytes.iter().take_while(|byte| **byte == b' ').count();
    if indentation > 3 {
        return None;
    }
    let marker = *bytes.get(indentation)?;
    if !matches!(marker, b'`' | b'~') {
        return None;
    }
    let length = bytes[indentation..]
        .iter()
        .take_while(|byte| **byte == marker)
        .count();
    if !(3..=MAX_MARKDOWN_FENCE_BYTES).contains(&length) {
        return None;
    }
    let remainder = &line[indentation + length..];
    if marker == b'`' && remainder.as_bytes().contains(&b'`') {
        return None;
    }
    Some(MarkdownFence {
        marker,
        length,
        start_byte,
    })
}

fn markdown_fence_closes(line: &str, fence: MarkdownFence) -> bool {
    let bytes = line.as_bytes();
    let indentation = bytes.iter().take_while(|byte| **byte == b' ').count();
    if indentation > 3 || bytes.get(indentation) != Some(&fence.marker) {
        return false;
    }
    let length = bytes[indentation..]
        .iter()
        .take_while(|byte| **byte == fence.marker)
        .count();
    length >= fence.length
        && length <= MAX_MARKDOWN_FENCE_BYTES
        && line[indentation + length..].trim().is_empty()
}

fn push_markdown_fence(
    blocks: &mut Vec<ParsedBlock>,
    source: &str,
    start: usize,
    end: usize,
    headings: &[String],
    retained: &mut RetainedOutputBudget,
) -> Result<()> {
    if start >= end || end > source.len() {
        return Err(TokenChunkError::ArithmeticOverflow);
    }
    retained.admit_record::<ParsedBlock>(end - start, headings)?;
    push_block(
        blocks,
        ParsedBlock {
            start_byte: start,
            end_byte: end,
            retrieval_text: source[start..end].to_owned(),
            kind: StructureKind::Code,
            path: headings.to_vec(),
            retrieval_maps_source: true,
        },
    )
}

fn push_markdown_body(
    blocks: &mut Vec<ParsedBlock>,
    source: &str,
    mut start: usize,
    mut end: usize,
    headings: &[String],
    retained: &mut RetainedOutputBudget,
) -> Result<()> {
    while start < end && matches!(source.as_bytes()[start], b'\r' | b'\n') {
        start += 1;
    }
    while start < end && matches!(source.as_bytes()[end - 1], b'\r' | b'\n') {
        end -= 1;
    }
    if start == end {
        return Ok(());
    }
    for (relative_start, relative_end) in paragraph_ranges(&source[start..end])? {
        let block_start = start
            .checked_add(relative_start)
            .ok_or(TokenChunkError::ArithmeticOverflow)?;
        let block_end = start
            .checked_add(relative_end)
            .ok_or(TokenChunkError::ArithmeticOverflow)?;
        let text = &source[block_start..block_end];
        retained.admit_record::<ParsedBlock>(text.len(), headings)?;
        push_block(
            blocks,
            ParsedBlock {
                start_byte: block_start,
                end_byte: block_end,
                retrieval_text: text.to_owned(),
                kind: markdown_kind(text),
                path: headings.to_vec(),
                retrieval_maps_source: true,
            },
        )?;
    }
    Ok(())
}

const PARSER_CHECKPOINT_INTERVAL_BYTES: usize = 4_096;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum HtmlTag {
    Heading(usize),
    Paragraph,
    ListItem,
    Preformatted,
    Code,
    Table,
}

impl HtmlTag {
    fn parse(name: &str) -> Option<Self> {
        if name.eq_ignore_ascii_case("h1") {
            Some(Self::Heading(1))
        } else if name.eq_ignore_ascii_case("h2") {
            Some(Self::Heading(2))
        } else if name.eq_ignore_ascii_case("h3") {
            Some(Self::Heading(3))
        } else if name.eq_ignore_ascii_case("h4") {
            Some(Self::Heading(4))
        } else if name.eq_ignore_ascii_case("h5") {
            Some(Self::Heading(5))
        } else if name.eq_ignore_ascii_case("h6") {
            Some(Self::Heading(6))
        } else if name.eq_ignore_ascii_case("p") {
            Some(Self::Paragraph)
        } else if name.eq_ignore_ascii_case("li") {
            Some(Self::ListItem)
        } else if name.eq_ignore_ascii_case("pre") {
            Some(Self::Preformatted)
        } else if name.eq_ignore_ascii_case("code") {
            Some(Self::Code)
        } else if name.eq_ignore_ascii_case("table") {
            Some(Self::Table)
        } else {
            None
        }
    }

    const fn kind(self) -> StructureKind {
        match self {
            Self::Heading(_) => StructureKind::Heading,
            Self::Paragraph => StructureKind::Paragraph,
            Self::ListItem => StructureKind::List,
            Self::Preformatted | Self::Code => StructureKind::Code,
            Self::Table => StructureKind::Table,
        }
    }
}

#[derive(Debug)]
struct OpenHtmlBlock {
    tag: HtmlTag,
    start_byte: usize,
    same_tag_depth: usize,
    retrieval_text: String,
}

#[derive(Debug, Default)]
struct HtmlFallbackBlock {
    start_byte: Option<usize>,
    end_byte: usize,
    retrieval_text: String,
    pending_whitespace: String,
}

impl HtmlFallbackBlock {
    fn push_visible(&mut self, start_byte: usize, end_byte: usize, character: char) {
        if character.is_whitespace() {
            if self.start_byte.is_some() {
                self.pending_whitespace.push(character);
            }
            return;
        }
        if self.start_byte.is_none() {
            self.start_byte = Some(start_byte);
        } else {
            self.retrieval_text.push_str(&self.pending_whitespace);
        }
        self.pending_whitespace.clear();
        self.retrieval_text.push(character);
        self.end_byte = end_byte;
    }
}

#[derive(Clone, Copy, Debug)]
struct HtmlToken {
    tag: Option<HtmlTag>,
    closing: bool,
    self_closing: bool,
    end_byte: usize,
}

pub(super) fn html_blocks(
    source: &str,
    checkpoint: &mut impl FnMut() -> bool,
) -> Result<Vec<ParsedBlock>> {
    let mut blocks = Vec::new();
    let mut retained = RetainedOutputBudget::default();
    let mut headings = Vec::<String>::new();
    let mut open = None::<OpenHtmlBlock>;
    let mut fallback = HtmlFallbackBlock::default();
    let mut next_checkpoint = 0usize;
    let mut cursor = 0;
    while cursor < source.len() {
        require_parser_checkpoint(cursor, &mut next_checkpoint, checkpoint)?;
        if source.as_bytes()[cursor] == b'<'
            && let Some(token) = scan_html_token(source, cursor, &mut next_checkpoint, checkpoint)?
        {
            if let Some(block) = open.as_ref()
                && token.closing
                && token.tag == Some(block.tag)
            {
                if block.same_tag_depth == 1 {
                    let Some(block) = open.take() else {
                        return Err(TokenChunkError::ArithmeticOverflow);
                    };
                    finish_html_block(
                        &mut blocks,
                        &mut headings,
                        block,
                        token.end_byte,
                        &mut retained,
                    )?;
                } else if let Some(block) = open.as_mut() {
                    block.same_tag_depth -= 1;
                }
            } else if let Some(block) = open.as_mut()
                && !token.closing
                && !token.self_closing
                && token.tag == Some(block.tag)
            {
                block.same_tag_depth = block
                    .same_tag_depth
                    .checked_add(1)
                    .ok_or(TokenChunkError::StructureLimitExceeded)?;
                if block.same_tag_depth > MAX_STRUCTURE_DEPTH {
                    return Err(TokenChunkError::StructureLimitExceeded);
                }
            } else if open.is_none()
                && !token.closing
                && !token.self_closing
                && let Some(tag) = token.tag
            {
                finish_html_fallback(&mut blocks, &headings, &mut fallback, &mut retained)?;
                open = Some(OpenHtmlBlock {
                    tag,
                    start_byte: cursor,
                    same_tag_depth: 1,
                    retrieval_text: String::new(),
                });
            }
            cursor = token.end_byte;
            continue;
        }

        let Some(character) = source[cursor..].chars().next() else {
            return Err(TokenChunkError::ArithmeticOverflow);
        };
        if let Some(block) = open.as_mut() {
            if character == '&'
                && let Some((decoded, end_byte)) = decode_html_entity(source, cursor)
            {
                block.retrieval_text.push(decoded);
                cursor = end_byte;
                continue;
            }
            block.retrieval_text.push(character);
        } else if character == '&'
            && let Some((decoded, end_byte)) = decode_html_entity(source, cursor)
        {
            fallback.push_visible(cursor, end_byte, decoded);
            cursor = end_byte;
            continue;
        } else {
            fallback.push_visible(cursor, cursor + character.len_utf8(), character);
        }
        cursor += character.len_utf8();
    }
    if let Some(block) = open {
        finish_html_block(
            &mut blocks,
            &mut headings,
            block,
            source.len(),
            &mut retained,
        )?;
    }
    finish_html_fallback(&mut blocks, &headings, &mut fallback, &mut retained)?;
    Ok(blocks)
}

fn scan_html_token(
    source: &str,
    start: usize,
    next_checkpoint: &mut usize,
    checkpoint: &mut impl FnMut() -> bool,
) -> Result<Option<HtmlToken>> {
    let bytes = source.as_bytes();
    let mut cursor = start + 1;
    let closing = bytes.get(cursor) == Some(&b'/');
    cursor += usize::from(closing);
    let name_start = cursor;
    while bytes.get(cursor).is_some_and(u8::is_ascii_alphanumeric) {
        require_parser_checkpoint(cursor, next_checkpoint, checkpoint)?;
        cursor += 1;
    }
    if cursor == name_start
        || !bytes
            .get(cursor)
            .is_some_and(|byte| byte.is_ascii_whitespace() || matches!(*byte, b'/' | b'>'))
    {
        return Ok(None);
    }
    let name = &source[name_start..cursor];
    let mut quote = None;
    while let Some(byte) = bytes.get(cursor).copied() {
        require_parser_checkpoint(cursor, next_checkpoint, checkpoint)?;
        match (quote, byte) {
            (Some(expected), found) if found == expected => quote = None,
            (None, b'\'' | b'"') => quote = Some(byte),
            (None, b'>') => {
                let self_closing = source[name_start..cursor].trim_end().ends_with('/');
                return Ok(Some(HtmlToken {
                    tag: HtmlTag::parse(name),
                    closing,
                    self_closing,
                    end_byte: cursor + 1,
                }));
            }
            _ => {}
        }
        cursor += 1;
    }
    Ok(None)
}

fn finish_html_block(
    blocks: &mut Vec<ParsedBlock>,
    headings: &mut Vec<String>,
    block: OpenHtmlBlock,
    end_byte: usize,
    retained: &mut RetainedOutputBudget,
) -> Result<()> {
    let retrieval_text = block.retrieval_text.trim();
    if retrieval_text.is_empty() {
        return Ok(());
    }
    if let HtmlTag::Heading(level) = block.tag {
        headings.truncate(level.saturating_sub(1));
        headings.push(retrieval_text.to_owned());
        validate_structure_path(headings)?;
    }
    retained.admit_record::<ParsedBlock>(retrieval_text.len(), headings)?;
    push_block(
        blocks,
        ParsedBlock {
            start_byte: block.start_byte,
            end_byte,
            retrieval_text: retrieval_text.to_owned(),
            kind: block.tag.kind(),
            path: headings.clone(),
            retrieval_maps_source: false,
        },
    )
}

fn finish_html_fallback(
    blocks: &mut Vec<ParsedBlock>,
    headings: &[String],
    fallback: &mut HtmlFallbackBlock,
    retained: &mut RetainedOutputBudget,
) -> Result<()> {
    let Some(start_byte) = fallback.start_byte.take() else {
        fallback.pending_whitespace.clear();
        return Ok(());
    };
    let end_byte = fallback.end_byte;
    let retrieval_text = std::mem::take(&mut fallback.retrieval_text);
    fallback.pending_whitespace.clear();
    retained.admit_record::<ParsedBlock>(retrieval_text.len(), headings)?;
    push_block(
        blocks,
        ParsedBlock {
            start_byte,
            end_byte,
            retrieval_text,
            kind: StructureKind::Paragraph,
            path: headings.to_vec(),
            retrieval_maps_source: false,
        },
    )
}

fn require_parser_checkpoint(
    cursor: usize,
    next_checkpoint: &mut usize,
    checkpoint: &mut impl FnMut() -> bool,
) -> Result<()> {
    if cursor < *next_checkpoint {
        return Ok(());
    }
    if !checkpoint() {
        Err(TokenChunkError::DeadlineExceeded)
    } else {
        *next_checkpoint = cursor.saturating_add(PARSER_CHECKPOINT_INTERVAL_BYTES);
        Ok(())
    }
}

fn decode_html_entity(source: &str, start: usize) -> Option<(char, usize)> {
    [
        ("&amp;", '&'),
        ("&lt;", '<'),
        ("&gt;", '>'),
        ("&quot;", '"'),
        ("&#39;", '\''),
    ]
    .into_iter()
    .find_map(|(encoded, decoded)| {
        source[start..]
            .starts_with(encoded)
            .then_some((decoded, start + encoded.len()))
    })
}

fn validate_structure_path(path: &[String]) -> Result<()> {
    if path.len() > MAX_STRUCTURE_DEPTH
        || path
            .iter()
            .any(|segment| segment.is_empty() || segment.len() > MAX_STRUCTURE_SEGMENT_BYTES)
    {
        return Err(TokenChunkError::StructureLimitExceeded);
    }
    Ok(())
}

fn paragraph_ranges(source: &str) -> Result<Vec<(usize, usize)>> {
    paragraph_ranges_with_checkpoint(source, &mut || true)
}

fn paragraph_ranges_with_checkpoint(
    source: &str,
    checkpoint: &mut impl FnMut() -> bool,
) -> Result<Vec<(usize, usize)>> {
    let mut ranges = Vec::new();
    let mut start = 0;
    let mut cursor = 0usize;
    let mut next_checkpoint = 0usize;
    while cursor < source.len() {
        require_parser_checkpoint(cursor, &mut next_checkpoint, checkpoint)?;
        if source.as_bytes().get(cursor..cursor.saturating_add(2)) == Some(b"\n\n") {
            if start < cursor {
                require_block_capacity(ranges.len())?;
                ranges.push((start, cursor));
            }
            cursor += 2;
            start = cursor;
        } else {
            cursor += 1;
        }
    }
    if start < source.len() {
        require_block_capacity(ranges.len())?;
        ranges.push((start, source.len()));
    }
    Ok(ranges)
}

fn push_block(blocks: &mut Vec<ParsedBlock>, block: ParsedBlock) -> Result<()> {
    require_block_capacity(blocks.len())?;
    blocks.push(block);
    Ok(())
}

fn require_block_capacity(current: usize) -> Result<()> {
    if current >= MAX_CHUNKS_PER_DOCUMENT {
        return Err(TokenChunkError::TooManyChunks {
            maximum: MAX_CHUNKS_PER_DOCUMENT,
        });
    }
    Ok(())
}

fn markdown_heading(line: &str) -> Option<(usize, &str)> {
    let level = line.bytes().take_while(|byte| *byte == b'#').count();
    if !(1..=6).contains(&level) || line.as_bytes().get(level) != Some(&b' ') {
        return None;
    }
    Some((level, line[level + 1..].trim()))
}

fn markdown_kind(text: &str) -> StructureKind {
    let trimmed = text.trim_start();
    if trimmed.starts_with("```") || trimmed.starts_with("    ") {
        StructureKind::Code
    } else if trimmed.starts_with("- ") || trimmed.starts_with("* ") {
        StructureKind::List
    } else if trimmed.starts_with('|') {
        StructureKind::Table
    } else {
        StructureKind::Paragraph
    }
}
