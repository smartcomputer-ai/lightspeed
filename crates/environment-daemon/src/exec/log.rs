//! Byte-offset output logs.
//!
//! A log holds a handle's combined output as segments addressed by byte
//! offset. `end` counts every byte ever appended; a retention cap drops
//! bytes but never renumbers them, so a page cut at any byte loses nothing
//! and every range the log no longer holds is reported as a typed gap.

use std::collections::VecDeque;

use environment_protocol::{
    bytes::ByteChunk,
    data::exec::{GapReason, OutputChunk, OutputGap, OutputStream},
};

/// Consecutive appends from one stream merge into one segment up to this
/// size, which keeps the chunk count of a page low.
const MERGE_LIMIT_BYTES: usize = 64 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
struct Segment {
    offset: u64,
    stream: OutputStream,
    data: Vec<u8>,
}

impl Segment {
    fn end(&self) -> u64 {
        self.offset + self.data.len() as u64
    }
}

/// One page cut from a log.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Page {
    pub chunks: Vec<OutputChunk>,
    pub gaps: Vec<OutputGap>,
    /// Offset after the last returned byte or gap; the start when the page
    /// is empty.
    pub next: u64,
    /// Bytes of `data` returned.
    pub bytes: u64,
}

impl Page {
    pub fn items(&self) -> u32 {
        u32::try_from(self.chunks.len() + self.gaps.len()).unwrap_or(u32::MAX)
    }
}

/// Retained output of one handle.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct OutputLog {
    /// Sorted by offset, disjoint.
    segments: VecDeque<Segment>,
    end: u64,
    retained: u64,
    /// Bytes below this offset were delivered and released.
    released_until: u64,
    /// Ranges `[start, end)` dropped from the middle, sorted, disjoint and
    /// never adjacent. Ranges below `released_until` are forgotten and then
    /// read as released.
    overflow: Vec<(u64, u64)>,
}

impl OutputLog {
    /// A log rebuilt from persisted records, in offset order. Holes between
    /// records, and a missing tail up to `end`, read as overflow.
    pub fn from_records(
        records: impl IntoIterator<Item = (u64, OutputStream, Vec<u8>)>,
        end: u64,
    ) -> Self {
        let mut log = Self::default();
        for (offset, stream, data) in records {
            if data.is_empty() {
                continue;
            }
            // Overlapping records (a tail that was compacted after a crash
            // left a stale copy) keep the first copy of every byte.
            let record_end = offset + data.len() as u64;
            if record_end <= log.end {
                continue;
            }
            let skip = log.end.saturating_sub(offset) as usize;
            let offset = offset.max(log.end);
            if offset > log.end {
                log.add_overflow(log.end, offset);
                log.end = offset;
            }
            log.append(stream, &data[skip..]);
        }
        if end > log.end {
            log.add_overflow(log.end, end);
            log.end = end;
        }
        log
    }

    /// Bytes produced so far.
    pub fn end(&self) -> u64 {
        self.end
    }

    /// Bytes held.
    pub fn retained_bytes(&self) -> u64 {
        self.retained
    }

    pub fn append(&mut self, stream: OutputStream, data: &[u8]) {
        if data.is_empty() {
            return;
        }
        let offset = self.end;
        match self.segments.back_mut() {
            Some(last)
                if last.stream == stream
                    && last.end() == offset
                    && last.data.len() + data.len() <= MERGE_LIMIT_BYTES =>
            {
                last.data.extend_from_slice(data);
            }
            _ => self.segments.push_back(Segment {
                offset,
                stream,
                data: data.to_vec(),
            }),
        }
        self.end += data.len() as u64;
        self.retained += data.len() as u64;
    }

    /// Keeps at most `cap` bytes. First releases delivered bytes (below
    /// `cursor`), oldest first; if still over, drops from the middle of the
    /// undelivered region, keeping its head (the first half of `cap` from
    /// `cursor`) and the newest bytes. Every dropped range becomes an
    /// overflow gap; consecutive drops extend one gap.
    pub fn enforce_cap(&mut self, cap: u64, cursor: u64) {
        while self.retained > cap {
            let Some(front) = self.segments.front_mut() else {
                break;
            };
            if front.offset >= cursor {
                break;
            }
            let releasable = cursor.min(front.end()) - front.offset;
            let release = releasable.min(self.retained - cap) as usize;
            front.data.drain(..release);
            front.offset += release as u64;
            self.retained -= release as u64;
            self.released_until = self.released_until.max(front.offset);
            if front.data.is_empty() {
                self.segments.pop_front();
            }
        }
        let released_until = self.released_until;
        self.overflow.retain(|(_, end)| *end > released_until);

        if self.retained <= cap {
            return;
        }
        let head_end = cursor.max(self.released_until).saturating_add(cap / 2);
        let mut excess = self.retained - cap;
        let mut index = self
            .segments
            .partition_point(|segment| segment.end() <= head_end);
        while excess > 0 && index < self.segments.len() {
            let segment = &mut self.segments[index];
            if segment.offset < head_end {
                // The segment straddles the head: keep its head part.
                let keep = (head_end - segment.offset) as usize;
                let rest = segment.data.split_off(keep);
                let tail = Segment {
                    offset: head_end,
                    stream: segment.stream,
                    data: rest,
                };
                self.segments.insert(index + 1, tail);
                index += 1;
                continue;
            }
            let drop = excess.min(segment.data.len() as u64);
            let start = segment.offset;
            segment.data.drain(..drop as usize);
            segment.offset += drop;
            let emptied = segment.data.is_empty();
            self.retained -= drop;
            excess -= drop;
            self.add_overflow(start, start + drop);
            if emptied {
                self.segments.remove(index);
            }
        }
    }

    /// A page from `start`: at most `max_bytes` of data, split at exactly
    /// that byte, and at most `max_items` chunks and gaps together. An empty
    /// budget returns an empty page at `start`.
    pub fn page(&self, start: u64, max_bytes: u64, max_items: u32) -> Page {
        let mut page = Page {
            next: start,
            ..Page::default()
        };
        let mut pos = start;
        let mut budget = max_bytes;
        while pos < self.end && budget > 0 && page.items() < max_items {
            if let Some(&(_, end)) = self
                .overflow
                .iter()
                .find(|(range_start, end)| *range_start <= pos && pos < *end)
            {
                push_gap(&mut page, pos, end, GapReason::Overflow, max_items);
                pos = end;
            } else if pos < self.released_until {
                let end = self
                    .overflow
                    .iter()
                    .map(|(range_start, _)| *range_start)
                    .find(|range_start| *range_start > pos)
                    .map_or(self.released_until, |range_start| {
                        range_start.min(self.released_until)
                    });
                push_gap(&mut page, pos, end, GapReason::Expired, max_items);
                pos = end;
            } else {
                let index = self
                    .segments
                    .partition_point(|segment| segment.end() <= pos);
                match self.segments.get(index) {
                    Some(segment) if segment.offset <= pos => {
                        let from = (pos - segment.offset) as usize;
                        let take = (segment.data.len() - from).min(budget as usize);
                        page.chunks.push(OutputChunk {
                            offset: pos,
                            stream: segment.stream,
                            data: ByteChunk::from(&segment.data[from..from + take]),
                        });
                        pos += take as u64;
                        budget -= take as u64;
                        page.bytes += take as u64;
                    }
                    next => {
                        // A hole no rule explains reads as overflow.
                        let end = next.map_or(self.end, |segment| segment.offset);
                        push_gap(&mut page, pos, end, GapReason::Overflow, max_items);
                        pos = end;
                    }
                }
            }
            page.next = pos;
        }
        page
    }

    fn add_overflow(&mut self, start: u64, end: u64) {
        if start >= end {
            return;
        }
        let index = self
            .overflow
            .partition_point(|(_, range_end)| *range_end < start);
        let mut merged = (start, end);
        while let Some(&(range_start, range_end)) = self.overflow.get(index) {
            if range_start > merged.1 {
                break;
            }
            merged = (merged.0.min(range_start), merged.1.max(range_end));
            self.overflow.remove(index);
        }
        self.overflow.insert(index, merged);
    }
}

fn push_gap(page: &mut Page, start: u64, end: u64, reason: GapReason, max_items: u32) {
    if let Some(last) = page.gaps.last_mut()
        && last.reason == reason
        && last.offset + last.omitted_bytes == start
        && page
            .chunks
            .last()
            .is_none_or(|chunk| chunk.offset < last.offset)
    {
        last.omitted_bytes += end - start;
        return;
    }
    debug_assert!(page.items() < max_items);
    page.gaps.push(OutputGap {
        offset: start,
        omitted_bytes: end - start,
        reason,
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bytes(page: &Page) -> Vec<u8> {
        page.chunks
            .iter()
            .flat_map(|chunk| chunk.data.as_slice().to_vec())
            .collect()
    }

    fn log_of(parts: &[&[u8]]) -> OutputLog {
        let mut log = OutputLog::default();
        for (index, part) in parts.iter().enumerate() {
            let stream = if index % 2 == 0 {
                OutputStream::Stdout
            } else {
                OutputStream::Stderr
            };
            log.append(stream, part);
        }
        log
    }

    #[test]
    fn pages_cut_at_max_bytes_and_continue_without_loss() {
        // Job paging used to skip the remainder of a cut chunk: every byte
        // of 20 000 must arrive through 100-byte pages.
        let data: Vec<u8> = (0..20_000u32).map(|i| (i % 251) as u8).collect();
        let mut log = OutputLog::default();
        for chunk in data.chunks(8192) {
            log.append(OutputStream::Stdout, chunk);
        }
        let mut collected = Vec::new();
        let mut next = 0;
        while next < log.end() {
            let page = log.page(next, 100, 4096);
            assert!(page.bytes <= 100);
            assert!(page.gaps.is_empty());
            assert_eq!(page.chunks[0].offset, next);
            collected.extend(bytes(&page));
            next = page.next;
        }
        assert_eq!(collected, data);
    }

    #[test]
    fn zero_budget_leaves_the_position_in_place() {
        let log = log_of(&[b"abc"]);
        let page = log.page(1, 0, 4096);
        assert!(page.chunks.is_empty() && page.gaps.is_empty());
        assert_eq!(page.next, 1);
    }

    #[test]
    fn chunks_keep_stream_and_offset() {
        let log = log_of(&[b"out", b"err"]);
        let page = log.page(2, 100, 4096);
        assert_eq!(page.chunks.len(), 2);
        assert_eq!(page.chunks[0].offset, 2);
        assert_eq!(page.chunks[0].stream, OutputStream::Stdout);
        assert_eq!(page.chunks[1].offset, 3);
        assert_eq!(page.chunks[1].stream, OutputStream::Stderr);
        assert_eq!(bytes(&page), b"terr");
        assert_eq!(page.next, 6);
    }

    #[test]
    fn cap_drops_one_middle_range_and_keeps_head_and_newest() {
        let mut log = OutputLog::default();
        for i in 0..64u8 {
            log.append(OutputStream::Stdout, &[i; 100]);
            log.enforce_cap(1000, 0);
        }
        assert_eq!(log.end(), 6400);
        assert_eq!(log.retained_bytes(), 1000);
        let page = log.page(0, 10_000, 4096);
        assert_eq!(page.gaps.len(), 1, "one contiguous gap: {:?}", page.gaps);
        assert_eq!(page.gaps[0].reason, GapReason::Overflow);
        assert_eq!(page.gaps[0].offset, 500);
        assert_eq!(page.gaps[0].omitted_bytes, 6400 - 1000);
        let data = bytes(&page);
        assert_eq!(
            &data[..500],
            &[[0u8; 100], [1; 100], [2; 100], [3; 100], [4; 100]].concat()[..]
        );
        assert_eq!(data[999], 63, "the newest byte survives");
        assert_eq!(page.next, 6400);
    }

    #[test]
    fn delivered_bytes_are_released_first_and_read_as_expired() {
        let mut log = log_of(&[b"0123456789"]);
        // Everything up to 8 was delivered through the cursor.
        log.append(OutputStream::Stdout, b"abcdef");
        log.enforce_cap(10, 8);
        assert_eq!(log.retained_bytes(), 10);
        let page = log.page(0, 100, 4096);
        assert_eq!(page.gaps[0].reason, GapReason::Expired);
        assert_eq!(page.gaps[0].offset, 0);
        assert_eq!(page.gaps[0].omitted_bytes, 6);
        assert_eq!(bytes(&page), b"6789abcdef");
    }

    #[test]
    fn page_item_limit_ends_the_page_early_without_loss() {
        let log = log_of(&[b"a", b"b", b"c", b"d"]);
        let page = log.page(0, 100, 2);
        assert_eq!(bytes(&page), b"ab");
        assert_eq!(page.next, 2);
        let rest = log.page(page.next, 100, 2);
        assert_eq!(bytes(&rest), b"cd");
    }

    #[test]
    fn records_rebuild_head_tail_and_the_gap_between() {
        let log = OutputLog::from_records(
            [
                (0, OutputStream::Stdout, b"head".to_vec()),
                (100, OutputStream::Stderr, b"tail".to_vec()),
            ],
            110,
        );
        assert_eq!(log.end(), 110);
        let page = log.page(0, 100, 4096);
        assert_eq!(bytes(&page), b"headtail");
        assert_eq!(page.gaps.len(), 2);
        assert_eq!((page.gaps[0].offset, page.gaps[0].omitted_bytes), (4, 96));
        assert_eq!((page.gaps[1].offset, page.gaps[1].omitted_bytes), (104, 6));
    }
}
