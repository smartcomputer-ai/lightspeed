//! Shaping command output pages into what the model sees: a head, the
//! bytes omitted after it, and a tail.
//!
//! Environment reads return byte-offset pages with typed gaps. A finished
//! command whose output exceeds the tool budget is shown as its first bytes
//! and its last bytes, with the omitted count in between, because failures
//! are reported at the end.

use environment_protocol::data::exec::{OutputChunk, OutputPage, OutputStream};

/// Output selected for the model, in offset order.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ShownOutput {
    /// Chunks before the first omitted range.
    pub head: Vec<OutputChunk>,
    /// The omitted ranges, merged: where the first starts, how many bytes
    /// all of them hold together, and how many of those the environment no
    /// longer holds.
    pub omitted: Option<Omission>,
    /// Chunks after the first omitted range.
    pub tail: Vec<OutputChunk>,
    /// Offset after the last byte or gap shown.
    pub next: u64,
    /// Bytes the command produced so far.
    pub end: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Omission {
    pub offset: u64,
    pub bytes: u64,
    /// Of `bytes`, those the environment's typed gaps report as gone
    /// (`overflow`, `expired`): reading them again returns the same gap.
    /// The rest was skipped here and can still be read.
    pub lost_bytes: u64,
}

impl ShownOutput {
    /// The first page of a read, with an optional second page that starts
    /// later (a tail read). Bytes of `tail` that `head` already returned
    /// are dropped; the range between the two pages and every gap inside
    /// them count as omitted.
    pub fn from_pages(head: OutputPage, tail: Option<OutputPage>) -> Self {
        let mut shown = Self::default();
        let mut items = Vec::new();
        let mut next = head.next;
        let mut end = head.end;
        push_page(&mut items, head, 0);
        if let Some(tail) = tail {
            let tail_start = page_start(&tail);
            if tail_start > next {
                items.push(Item::Gap {
                    offset: next,
                    bytes: tail_start - next,
                    lost: false,
                });
            }
            let skip_before = next;
            next = next.max(tail.next);
            end = end.max(tail.end);
            push_page(&mut items, tail, skip_before);
        }
        shown.next = next;
        shown.end = end;
        for item in items {
            match item {
                Item::Chunk(chunk) if shown.omitted.is_none() => shown.head.push(chunk),
                Item::Chunk(chunk) => shown.tail.push(chunk),
                Item::Gap {
                    offset,
                    bytes,
                    lost,
                } => {
                    let lost_bytes = if lost { bytes } else { 0 };
                    match &mut shown.omitted {
                        Some(omission) => {
                            omission.bytes += bytes;
                            omission.lost_bytes += lost_bytes;
                        }
                        None => {
                            shown.omitted = Some(Omission {
                                offset,
                                bytes,
                                lost_bytes,
                            })
                        }
                    }
                }
            }
        }
        shown
    }

    /// Keep at most `budget` bytes: the first half of the budget from the
    /// head and the rest from the end of what was read, omitting the middle.
    /// Used when a single page returned more than the model may see.
    pub fn trim_to(mut self, budget: u64) -> Self {
        let total = self.shown_bytes();
        if total <= budget {
            return self;
        }
        let head_budget = budget / 2;
        let tail_budget = budget - head_budget;
        let mut chunks: Vec<OutputChunk> = self.head.drain(..).chain(self.tail.drain(..)).collect();
        let first = chunks
            .first()
            .map(|chunk| chunk.offset)
            .unwrap_or(self.next);
        let mut head = Vec::new();
        let mut taken = 0u64;
        let mut cut_at = first;
        let mut index = 0;
        while index < chunks.len() && taken < head_budget {
            let chunk = &mut chunks[index];
            let room = head_budget - taken;
            let length = chunk.data.len() as u64;
            if length <= room {
                taken += length;
                cut_at = chunk.offset + length;
                head.push(chunk.clone());
                index += 1;
            } else {
                head.push(slice_chunk(chunk, 0, room));
                cut_at = chunk.offset + room;
                taken = head_budget;
            }
        }
        let mut tail = Vec::new();
        let mut kept = 0u64;
        for chunk in chunks.iter().rev() {
            if kept >= tail_budget {
                break;
            }
            let length = chunk.data.len() as u64;
            let chunk_end = chunk.offset + length;
            let start = chunk.offset.max(cut_at);
            if chunk_end <= start {
                break;
            }
            let available = chunk_end - start;
            let keep = available.min(tail_budget - kept);
            let from = chunk_end - keep - chunk.offset;
            tail.push(slice_chunk(chunk, from, keep));
            kept += keep;
        }
        tail.reverse();
        let dropped = total - taken - kept;
        let omitted = self.omitted_bytes() + dropped;
        let offset = self
            .omitted
            .map_or(cut_at, |omission| omission.offset.min(cut_at));
        self.head = head;
        self.tail = tail;
        let lost_bytes = self.lost_bytes();
        self.omitted = (omitted > 0).then_some(Omission {
            offset,
            bytes: omitted,
            lost_bytes,
        });
        self
    }

    pub fn shown_bytes(&self) -> u64 {
        self.head
            .iter()
            .chain(&self.tail)
            .map(|chunk| chunk.data.len() as u64)
            .sum()
    }

    pub fn omitted_bytes(&self) -> u64 {
        self.omitted.map_or(0, |omission| omission.bytes)
    }

    /// Omitted bytes the environment no longer holds.
    pub fn lost_bytes(&self) -> u64 {
        self.omitted.map_or(0, |omission| omission.lost_bytes)
    }
}

enum Item {
    Chunk(OutputChunk),
    /// `lost`: a gap the environment reported; otherwise a readable range
    /// between two pages.
    Gap {
        offset: u64,
        bytes: u64,
        lost: bool,
    },
}

/// Where a page begins: its first chunk or gap, or `next` when empty.
fn page_start(page: &OutputPage) -> u64 {
    let first_chunk = page.chunks.first().map(|chunk| chunk.offset);
    let first_gap = page.gaps.first().map(|gap| gap.offset);
    match (first_chunk, first_gap) {
        (Some(chunk), Some(gap)) => chunk.min(gap),
        (Some(offset), None) | (None, Some(offset)) => offset,
        (None, None) => page.next,
    }
}

/// Append a page's chunks and gaps in offset order, dropping bytes before
/// `skip_before`.
fn push_page(items: &mut Vec<Item>, page: OutputPage, skip_before: u64) {
    let mut entries: Vec<Item> = page
        .chunks
        .into_iter()
        .map(Item::Chunk)
        .chain(page.gaps.into_iter().map(|gap| Item::Gap {
            offset: gap.offset,
            bytes: gap.omitted_bytes,
            lost: true,
        }))
        .collect();
    entries.sort_by_key(|item| match item {
        Item::Chunk(chunk) => chunk.offset,
        Item::Gap { offset, .. } => *offset,
    });
    for item in entries {
        match item {
            Item::Chunk(chunk) => {
                let length = chunk.data.len() as u64;
                if chunk.offset + length <= skip_before {
                    continue;
                }
                if chunk.offset < skip_before {
                    let from = skip_before - chunk.offset;
                    items.push(Item::Chunk(slice_chunk(&chunk, from, length - from)));
                } else {
                    items.push(Item::Chunk(chunk));
                }
            }
            Item::Gap {
                offset,
                bytes,
                lost,
            } => {
                let gap_end = offset + bytes;
                if gap_end <= skip_before {
                    continue;
                }
                let start = offset.max(skip_before);
                items.push(Item::Gap {
                    offset: start,
                    bytes: gap_end - start,
                    lost,
                });
            }
        }
    }
}

fn slice_chunk(chunk: &OutputChunk, from: u64, length: u64) -> OutputChunk {
    let from = from as usize;
    let to = from + length as usize;
    OutputChunk {
        offset: chunk.offset + from as u64,
        stream: chunk.stream,
        data: chunk.data.as_slice()[from..to].to_vec().into(),
    }
}

/// Whether a chunk belongs to standard error rather than standard output
/// (a PTY merges both into standard output).
pub fn is_stderr(stream: OutputStream) -> bool {
    matches!(stream, OutputStream::Stderr)
}

#[cfg(test)]
mod tests {
    use environment_protocol::data::exec::{GapReason, OutputGap};

    use super::*;

    fn chunk(offset: u64, text: &str) -> OutputChunk {
        OutputChunk {
            offset,
            stream: OutputStream::Stdout,
            data: text.as_bytes().to_vec().into(),
        }
    }

    fn text(chunks: &[OutputChunk]) -> String {
        chunks
            .iter()
            .map(|chunk| String::from_utf8_lossy(chunk.data.as_slice()).into_owned())
            .collect()
    }

    #[test]
    fn head_and_tail_pages_show_the_skipped_range() {
        let head = OutputPage {
            chunks: vec![chunk(0, "head")],
            next: 4,
            end: 100,
            gaps: vec![],
        };
        let tail = OutputPage {
            chunks: vec![chunk(96, "tail")],
            next: 100,
            end: 100,
            gaps: vec![],
        };
        let shown = ShownOutput::from_pages(head, Some(tail));
        assert_eq!(text(&shown.head), "head");
        assert_eq!(text(&shown.tail), "tail");
        assert_eq!(
            shown.omitted,
            Some(Omission {
                offset: 4,
                bytes: 92,
                lost_bytes: 0,
            })
        );
        assert_eq!((shown.next, shown.end), (100, 100));
    }

    #[test]
    fn overlapping_pages_show_every_byte_once() {
        let head = OutputPage {
            chunks: vec![chunk(0, "abcdef")],
            next: 6,
            end: 8,
            gaps: vec![],
        };
        let tail = OutputPage {
            chunks: vec![chunk(4, "efgh")],
            next: 8,
            end: 8,
            gaps: vec![],
        };
        let shown = ShownOutput::from_pages(head, Some(tail));
        assert_eq!(text(&shown.head), "abcdefgh");
        assert!(shown.omitted.is_none());
    }

    #[test]
    fn daemon_gaps_count_as_omitted() {
        let page = OutputPage {
            chunks: vec![chunk(0, "ab"), chunk(10, "yz")],
            next: 12,
            end: 12,
            gaps: vec![OutputGap {
                offset: 2,
                omitted_bytes: 8,
                reason: GapReason::Overflow,
            }],
        };
        let shown = ShownOutput::from_pages(page, None);
        assert_eq!(text(&shown.head), "ab");
        assert_eq!(text(&shown.tail), "yz");
        assert_eq!(shown.omitted_bytes(), 8);
        assert_eq!(
            shown.lost_bytes(),
            8,
            "an overflow gap cannot be read again"
        );
    }

    #[test]
    fn retention_gaps_and_skipped_ranges_are_told_apart() {
        let head = OutputPage {
            chunks: vec![chunk(0, "head")],
            next: 14,
            end: 100,
            gaps: vec![OutputGap {
                offset: 4,
                omitted_bytes: 10,
                reason: GapReason::Overflow,
            }],
        };
        let tail = OutputPage {
            chunks: vec![chunk(96, "tail")],
            next: 100,
            end: 100,
            gaps: vec![],
        };
        let shown = ShownOutput::from_pages(head, Some(tail));
        assert_eq!(
            shown.omitted,
            Some(Omission {
                offset: 4,
                bytes: 92,
                lost_bytes: 10,
            })
        );
    }

    #[test]
    fn trimming_keeps_the_first_and_last_bytes() {
        let page = OutputPage {
            chunks: vec![chunk(0, "0123456789"), chunk(10, "abcdefghij")],
            next: 20,
            end: 20,
            gaps: vec![],
        };
        let shown = ShownOutput::from_pages(page, None).trim_to(8);
        assert_eq!(text(&shown.head), "0123");
        assert_eq!(text(&shown.tail), "ghij");
        assert_eq!(
            shown.omitted,
            Some(Omission {
                offset: 4,
                bytes: 12,
                lost_bytes: 0,
            })
        );
        assert_eq!(shown.shown_bytes(), 8);
    }
}
