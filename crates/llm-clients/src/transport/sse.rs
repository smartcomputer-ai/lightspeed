//! Incremental Server-Sent Events parser.

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SseEvent {
    pub event: Option<String>,
    pub data: String,
    pub id: Option<String>,
    pub retry: Option<u64>,
}

impl SseEvent {
    pub fn is_empty(&self) -> bool {
        self.event.is_none() && self.data.is_empty() && self.id.is_none() && self.retry.is_none()
    }
}

#[derive(Debug, Default)]
pub struct SseParser {
    buffer: String,
    current: SseEvent,
}

impl SseParser {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, chunk: &str) -> Vec<SseEvent> {
        self.buffer.push_str(chunk);
        let mut events = Vec::new();

        while let Some(pos) = self.buffer.find('\n') {
            let mut line = self.buffer[..pos].to_string();
            let consumed = pos + 1;
            self.buffer.drain(..consumed);
            if line.ends_with('\r') {
                line.pop();
            }
            self.process_line(&line, &mut events);
        }

        events
    }

    pub fn finish(mut self) -> Option<SseEvent> {
        if !self.buffer.is_empty() {
            let line = std::mem::take(&mut self.buffer);
            let line = line.trim_end_matches(['\r', '\n']);
            let mut ignored = Vec::new();
            self.process_line(line, &mut ignored);
        }

        if self.current.is_empty() {
            None
        } else {
            Some(self.current)
        }
    }

    fn process_line(&mut self, line: &str, events: &mut Vec<SseEvent>) {
        if line.is_empty() {
            if !self.current.is_empty() {
                events.push(std::mem::take(&mut self.current));
            }
            return;
        }

        if line.starts_with(':') {
            return;
        }

        let mut parts = line.splitn(2, ':');
        let field = parts.next().unwrap_or("");
        let value = parts.next().unwrap_or("");
        let value = value.strip_prefix(' ').unwrap_or(value);

        match field {
            "event" => self.current.event = Some(value.to_string()),
            "data" => {
                if !self.current.data.is_empty() {
                    self.current.data.push('\n');
                }
                self.current.data.push_str(value);
            }
            "id" => self.current.id = Some(value.to_string()),
            "retry" => {
                if let Ok(retry) = value.parse::<u64>() {
                    self.current.retry = Some(retry);
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_multiline_data_event() {
        let mut parser = SseParser::new();
        let events = parser.push("event: message\ndata: hello\ndata: world\n\n");

        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event.as_deref(), Some("message"));
        assert_eq!(events[0].data, "hello\nworld");
    }

    #[test]
    fn ignores_comments_and_unknown_fields() {
        let mut parser = SseParser::new();
        let events = parser.push(": ping\nunknown: x\ndata: ok\n\n");

        assert_eq!(events.len(), 1);
        assert_eq!(events[0].data, "ok");
    }

    #[test]
    fn handles_id_retry_and_split_chunks() {
        let mut parser = SseParser::new();
        assert!(parser.push("id: 1\nret").is_empty());
        let events = parser.push("ry: 1500\ndata: ok\n\n");

        assert_eq!(events.len(), 1);
        assert_eq!(events[0].id.as_deref(), Some("1"));
        assert_eq!(events[0].retry, Some(1500));
        assert_eq!(events[0].data, "ok");
    }

    #[test]
    fn finish_flushes_trailing_event_without_blank_line() {
        let mut parser = SseParser::new();
        assert!(parser.push("event: done\ndata: {\"ok\":true}").is_empty());

        let event = parser.finish().expect("trailing event");
        assert_eq!(event.event.as_deref(), Some("done"));
        assert_eq!(event.data, "{\"ok\":true}");
    }
}
