//! A streaming reader for `multipart/related` responses.
//!
//! A study can be many gigabytes, so parts go to the sink as they arrive and
//! only a delimiter's length of data is ever held back. A gateway is untrusted
//! input: nothing it says about a part is used except where the part ends, and
//! headers and the number of parts are bounded.

use std::io;

/// More than any real part's headers.
const MOST_HEADER_BYTES: usize = 64 * 1024;
/// PS3.18 has no limit, and a study has to fit on a disk.
pub const MOST_PARTS: usize = 100_000;

/// What a response's Content-Type says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Framing {
    pub boundary: String,
    /// Allowed deviations this response has, to report to the server's vendor.
    pub deviations: Vec<&'static str>,
}

/// Read a `multipart/related` Content-Type. Tolerates the two deviations seen
/// in the field: no `type` parameter, and a boundary quoted when it need not be.
pub fn framing(content_type: &str) -> Option<Framing> {
    let mut pieces = split_parameters(content_type);
    let essence = pieces.next()?.trim().to_ascii_lowercase();
    if essence != "multipart/related" {
        return None;
    }
    let mut boundary = None;
    let mut deviations = Vec::new();
    let mut has_type = false;
    for piece in pieces {
        let Some((key, value)) = piece.split_once('=') else {
            continue;
        };
        let value = value.trim();
        match key.trim().to_ascii_lowercase().as_str() {
            "boundary" => {
                let unquoted = value.strip_prefix('"').and_then(|v| v.strip_suffix('"'));
                if let Some(inner) = unquoted {
                    if inner.bytes().all(token_byte) {
                        deviations.push("a boundary quoted when it need not be");
                    }
                }
                boundary = Some(unquoted.unwrap_or(value).to_string());
            }
            "type" => has_type = true,
            _ => {}
        }
    }
    if !has_type {
        deviations.push("no type parameter");
    }
    let boundary = boundary.filter(|b| !b.is_empty() && b.len() <= 200)?;
    Some(Framing { boundary, deviations })
}

fn token_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b)
}

/// Split on semicolons outside quotes.
fn split_parameters(text: &str) -> impl Iterator<Item = &str> {
    let mut rest = Some(text);
    std::iter::from_fn(move || {
        let current = rest?;
        let mut quoted = false;
        for (i, c) in current.char_indices() {
            match c {
                '"' => quoted = !quoted,
                ';' if !quoted => {
                    rest = Some(&current[i + 1..]);
                    return Some(&current[..i]);
                }
                _ => {}
            }
        }
        rest = None;
        Some(current)
    })
}

/// Where a part's bytes go.
pub trait Sink {
    /// `content_type` is the part's own, lower case, when it has one.
    fn begin(&mut self, content_type: Option<&str>) -> io::Result<()>;
    fn data(&mut self, bytes: &[u8]) -> io::Result<()>;
    /// The part ended properly.
    fn end(&mut self) -> io::Result<()>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    /// Before the first delimiter.
    Preamble,
    /// Just after a delimiter, deciding whether it was the last.
    AfterDelimiter,
    Headers,
    Body,
    Done,
}

/// Why a response cannot be read further.
#[derive(Debug)]
pub enum ReadError {
    Sink(io::Error),
    TooManyParts,
    BadHeaders,
}

impl std::fmt::Display for ReadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ReadError::Sink(e) => write!(f, "{e}"),
            ReadError::TooManyParts => write!(f, "too many parts"),
            ReadError::BadHeaders => write!(f, "a part's headers could not be read"),
        }
    }
}

/// The reader, fed chunks as they arrive.
pub struct Reader {
    delimiter: Vec<u8>,
    held: Vec<u8>,
    state: State,
    parts: usize,
    most_parts: usize,
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    let first = needle[0];
    let last = haystack.len() - needle.len();
    let mut at = 0;
    while at <= last {
        let hop = haystack[at..=last].iter().position(|b| *b == first)?;
        at += hop;
        if &haystack[at..at + needle.len()] == needle {
            return Some(at);
        }
        at += 1;
    }
    None
}

impl Reader {
    pub fn new(boundary: &str) -> Self {
        let delimiter = [b"\r\n--", boundary.as_bytes()].concat();
        Self {
            delimiter,
            // The first delimiter need not follow a line break.
            held: b"\r\n".to_vec(),
            state: State::Preamble,
            parts: 0,
            most_parts: MOST_PARTS,
        }
    }

    /// A lower limit on parts than the standard one.
    pub fn with_most_parts(mut self, most: usize) -> Self {
        self.most_parts = most.min(MOST_PARTS);
        self
    }

    /// Parts begun so far.
    pub fn parts(&self) -> usize {
        self.parts
    }

    /// True once the closing delimiter has been read.
    pub fn finished(&self) -> bool {
        self.state == State::Done
    }

    /// True when a part was begun and has not ended.
    pub fn inside_part(&self) -> bool {
        self.state == State::Body
    }

    pub fn feed(&mut self, chunk: &[u8], sink: &mut dyn Sink) -> Result<(), ReadError> {
        self.held.extend_from_slice(chunk);
        let mut used = 0;
        loop {
            let view = &self.held[used..];
            match self.state {
                State::Done => {
                    used = self.held.len();
                    break;
                }
                State::Preamble => match find(view, &self.delimiter) {
                    Some(at) => {
                        used += at + self.delimiter.len();
                        self.state = State::AfterDelimiter;
                    }
                    None => {
                        used += view.len().saturating_sub(self.delimiter.len() - 1);
                        break;
                    }
                },
                State::AfterDelimiter => {
                    if view.len() < 2 {
                        break;
                    }
                    if view.starts_with(b"--") {
                        self.state = State::Done;
                        continue;
                    }
                    // Transport padding, then the end of the delimiter's line.
                    let Some(end) = find(view, b"\r\n") else {
                        if view.len() > MOST_HEADER_BYTES {
                            return Err(ReadError::BadHeaders);
                        }
                        break;
                    };
                    used += end + 2;
                    self.state = State::Headers;
                }
                State::Headers => {
                    // A part with no headers has an empty line straight away.
                    let (length, skip) = if view.starts_with(b"\r\n") {
                        (0, 2)
                    } else {
                        match find(view, b"\r\n\r\n") {
                            Some(at) => (at, 4),
                            None if view.len() > MOST_HEADER_BYTES => return Err(ReadError::BadHeaders),
                            None => break,
                        }
                    };
                    if length > MOST_HEADER_BYTES {
                        return Err(ReadError::BadHeaders);
                    }
                    if self.parts >= self.most_parts {
                        return Err(ReadError::TooManyParts);
                    }
                    let content_type = header(&view[..length], "content-type");
                    sink.begin(content_type.as_deref()).map_err(ReadError::Sink)?;
                    self.parts += 1;
                    used += length + skip;
                    self.state = State::Body;
                }
                State::Body => match find(view, &self.delimiter) {
                    Some(at) => {
                        sink.data(&view[..at]).map_err(ReadError::Sink)?;
                        sink.end().map_err(ReadError::Sink)?;
                        used += at + self.delimiter.len();
                        self.state = State::AfterDelimiter;
                    }
                    None => {
                        // The start of a delimiter may be at the very end.
                        let safe = view.len().saturating_sub(self.delimiter.len() - 1);
                        sink.data(&view[..safe]).map_err(ReadError::Sink)?;
                        used += safe;
                        break;
                    }
                },
            }
        }
        self.held.drain(..used);
        Ok(())
    }
}

/// One header's value, lower case. Anything else a part says is not used.
fn header(block: &[u8], name: &str) -> Option<String> {
    let text = String::from_utf8_lossy(block);
    text.split("\r\n").find_map(|line| {
        let (key, value) = line.split_once(':')?;
        key.trim()
            .eq_ignore_ascii_case(name)
            .then(|| value.trim().to_ascii_lowercase())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct Collect {
        parts: Vec<(Option<String>, Vec<u8>)>,
        open: bool,
    }

    impl Sink for Collect {
        fn begin(&mut self, content_type: Option<&str>) -> io::Result<()> {
            self.parts.push((content_type.map(String::from), Vec::new()));
            self.open = true;
            Ok(())
        }
        fn data(&mut self, bytes: &[u8]) -> io::Result<()> {
            self.parts.last_mut().unwrap().1.extend_from_slice(bytes);
            Ok(())
        }
        fn end(&mut self) -> io::Result<()> {
            self.open = false;
            Ok(())
        }
    }

    const BODY: &[u8] =
        b"preamble text\r\n--B\r\nContent-Type: application/dicom\r\nContent-Location: ..\\..\\evil\r\n\r\n\
        one\r\n--not-the-boundary\r\nstill one\r\n--B\r\nContent-Type: Application/DICOM\r\n\r\n\r\n--B--\r\nepilogue";

    fn read(chunks: &[&[u8]]) -> (Reader, Collect) {
        let mut reader = Reader::new("B");
        let mut sink = Collect::default();
        for chunk in chunks {
            reader.feed(chunk, &mut sink).unwrap();
        }
        (reader, sink)
    }

    #[test]
    fn parts_come_out_whole() {
        let (reader, sink) = read(&[BODY]);
        assert!(reader.finished());
        assert_eq!(sink.parts.len(), 2);
        assert_eq!(sink.parts[0].1, b"one\r\n--not-the-boundary\r\nstill one");
        assert_eq!(sink.parts[0].0.as_deref(), Some("application/dicom"));
        assert!(sink.parts[1].1.is_empty());
        assert_eq!(sink.parts[1].0.as_deref(), Some("application/dicom"));
        assert!(!sink.open);
    }

    #[test]
    fn a_split_anywhere_changes_nothing() {
        let (_, whole) = read(&[BODY]);
        for at in 0..BODY.len() {
            let (reader, sink) = read(&[&BODY[..at], &BODY[at..]]);
            assert!(reader.finished(), "split at {at}");
            assert_eq!(sink.parts, whole.parts, "split at {at}");
        }
        let bytes: Vec<&[u8]> = BODY.chunks(1).collect();
        assert_eq!(read(&bytes).1.parts, whole.parts);
    }

    #[test]
    fn a_response_that_stops_short_is_seen_to() {
        let inside = find(BODY, b"still").unwrap();
        let cut = &BODY[..inside];
        let (reader, sink) = read(&[cut]);
        assert!(!reader.finished());
        assert!(reader.inside_part());
        assert!(sink.open);
    }

    #[test]
    fn little_is_held_back_while_a_large_part_flows() {
        let mut reader = Reader::new("B");
        let mut sink = Collect::default();
        reader
            .feed(b"--B\r\nContent-Type: application/dicom\r\n\r\n", &mut sink)
            .unwrap();
        for _ in 0..1000 {
            reader.feed(&[7u8; 4096], &mut sink).unwrap();
            assert!(reader.held.len() < 16);
        }
    }

    #[test]
    fn parts_are_counted_and_stopped() {
        let mut reader = Reader::new("B");
        let mut sink = Collect::default();
        reader.parts = MOST_PARTS;
        let result = reader.feed(b"--B\r\n\r\nx\r\n--B--", &mut sink);
        assert!(matches!(result, Err(ReadError::TooManyParts)));
    }

    #[test]
    fn endless_headers_are_refused() {
        let mut reader = Reader::new("B");
        let mut sink = Collect::default();
        reader.feed(b"--B\r\n", &mut sink).unwrap();
        let result = reader.feed(&vec![b'a'; MOST_HEADER_BYTES + 10], &mut sink);
        assert!(matches!(result, Err(ReadError::BadHeaders)));
    }

    #[test]
    fn content_types_are_read_tolerantly() {
        let f = framing("multipart/related; type=\"application/dicom\"; boundary=abc").unwrap();
        assert_eq!(f.boundary, "abc");
        assert!(f.deviations.is_empty());

        let f = framing("Multipart/Related; boundary=\"abc\"").unwrap();
        assert_eq!(f.boundary, "abc");
        assert_eq!(f.deviations.len(), 2);

        let f = framing("multipart/related; type=\"a;b\"; boundary=\"a b\"").unwrap();
        assert_eq!(f.boundary, "a b");
        assert!(f.deviations.is_empty(), "a space does need quotes");

        assert_eq!(framing("text/html; charset=utf-8"), None);
        assert_eq!(framing("multipart/related; type=\"application/dicom\""), None);
    }
}
