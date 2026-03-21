use async_stream::try_stream;
use futures::{Stream, StreamExt};
use reqwest::Response;

use crate::MuAiError;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SseFrame {
    pub event: Option<String>,
    pub data: String,
}

pub fn stream_sse_frames(
    response: Response,
) -> impl Stream<Item = Result<SseFrame, MuAiError>> + Send + 'static {
    try_stream! {
        let mut decoder = SseDecoder::default();
        let mut bytes = response.bytes_stream();
        while let Some(chunk) = bytes.next().await {
            let chunk = chunk?;
            for frame in decoder.push(chunk.as_ref())? {
                yield frame;
            }
        }

        for frame in decoder.finish()? {
            yield frame;
        }
    }
}

#[derive(Default)]
struct SseDecoder {
    buffer: Vec<u8>,
}

impl SseDecoder {
    fn push(&mut self, chunk: &[u8]) -> Result<Vec<SseFrame>, MuAiError> {
        self.buffer.extend_from_slice(chunk);
        self.take_frames()
    }

    fn finish(&mut self) -> Result<Vec<SseFrame>, MuAiError> {
        if self.buffer.is_empty() {
            return Ok(Vec::new());
        }

        let frame = parse_frame(&self.buffer)?;
        self.buffer.clear();
        Ok(frame.into_iter().collect())
    }

    fn take_frames(&mut self) -> Result<Vec<SseFrame>, MuAiError> {
        let mut frames = Vec::new();
        while let Some((frame_end, delimiter_len)) = find_frame_end(&self.buffer) {
            let frame_bytes = self.buffer[..frame_end].to_vec();
            self.buffer.drain(..frame_end + delimiter_len);
            if let Some(frame) = parse_frame(&frame_bytes)? {
                frames.push(frame);
            }
        }
        Ok(frames)
    }
}

fn find_frame_end(buffer: &[u8]) -> Option<(usize, usize)> {
    let mut index = 0usize;
    while index < buffer.len() {
        if buffer.get(index..index + 4) == Some(b"\r\n\r\n") {
            return Some((index, 4));
        }
        if buffer.get(index..index + 2) == Some(b"\n\n")
            || buffer.get(index..index + 2) == Some(b"\r\r")
        {
            return Some((index, 2));
        }
        index += 1;
    }
    None
}

fn parse_frame(bytes: &[u8]) -> Result<Option<SseFrame>, MuAiError> {
    let text = String::from_utf8(bytes.to_vec())
        .map_err(|error| MuAiError::InvalidSseFrame(format!("non utf-8 frame: {error}")))?;
    let mut event = None;
    let mut data = Vec::new();
    for raw_line in text.lines() {
        let line = raw_line.trim_end_matches('\r');
        if line.is_empty() || line.starts_with(':') {
            continue;
        }
        if let Some(rest) = line.strip_prefix("event:") {
            event = Some(rest.trim_start().to_string());
            continue;
        }
        if let Some(rest) = line.strip_prefix("data:") {
            data.push(rest.trim_start().to_string());
        }
    }

    if event.is_none() && data.is_empty() {
        return Ok(None);
    }

    Ok(Some(SseFrame {
        event,
        data: data.join("\n"),
    }))
}

#[cfg(test)]
mod tests {
    use super::{find_frame_end, parse_frame, SseDecoder, SseFrame};

    #[test]
    fn parses_event_and_data() {
        let frame = match parse_frame(b"event: delta\ndata: {\"hello\":1}\n") {
            Ok(Some(value)) => value,
            Ok(None) => panic!("expected frame"),
            Err(error) => panic!("expected frame, got error: {error}"),
        };
        assert_eq!(
            frame,
            SseFrame {
                event: Some("delta".to_string()),
                data: "{\"hello\":1}".to_string(),
            }
        );
    }

    #[test]
    fn finds_frame_with_crlf() {
        assert_eq!(find_frame_end(b"data: one\r\n\r\nrest"), Some((9, 4)));
    }

    #[test]
    fn handles_split_chunks() {
        let mut decoder = SseDecoder::default();
        let frames = match decoder.push(b"data: hel") {
            Ok(value) => value,
            Err(error) => panic!("unexpected error: {error}"),
        };
        assert!(frames.is_empty());
        let frames = match decoder.push(b"lo\ndata: world\n\n") {
            Ok(value) => value,
            Err(error) => panic!("unexpected error: {error}"),
        };
        assert_eq!(
            frames,
            vec![SseFrame {
                event: None,
                data: "hello\nworld".to_string(),
            }]
        );
    }
}
