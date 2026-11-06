#![no_main]
//! A DICOMweb reply comes from whoever runs the gateway. Reading one must not
//! panic, however it is split, and a split must not change what is read.

use libfuzzer_sys::fuzz_target;
use ruthos_net::dicomweb::multipart::{framing, Reader, Sink};

#[derive(Default, PartialEq, Debug)]
struct Collect {
    parts: Vec<Vec<u8>>,
}

impl Sink for Collect {
    fn begin(&mut self, _content_type: Option<&str>) -> std::io::Result<()> {
        self.parts.push(Vec::new());
        Ok(())
    }
    fn data(&mut self, bytes: &[u8]) -> std::io::Result<()> {
        if let Some(part) = self.parts.last_mut() {
            part.extend_from_slice(bytes);
        }
        Ok(())
    }
    fn end(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn read(chunks: &[&[u8]]) -> Option<Collect> {
    let mut reader = Reader::new("B");
    let mut sink = Collect::default();
    for chunk in chunks {
        reader.feed(chunk, &mut sink).ok()?;
    }
    Some(sink)
}

fuzz_target!(|data: &[u8]| {
    let _ = framing(&String::from_utf8_lossy(data));
    let at = data.len() / 2;
    assert_eq!(read(&[data]), read(&[&data[..at], &data[at..]]));
});
