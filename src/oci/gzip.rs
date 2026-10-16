//! Gzip for layers, with output that doesn't depend on how its input is
//! split.
//!
//! We stream layers through the compressor, so its output must be a
//! function of the bytes alone, not of how `tar` happens to chunk its
//! writes. zlib-rs, flate2's backend here, doesn't promise that: the same
//! bytes written in different pieces can compress differently. So we hand
//! it the stream in fixed-size chunks, whatever the writes above look like.

use std::io::{self, Write};

use flate2::Compression;
use flate2::write::GzEncoder;

/// Size of the pieces the compressor sees.
const CHUNK: usize = 1024 * 1024;

/// A gzip compressor writing to `inner`. Call [`GzipWriter::finish`] to end
/// the stream; dropping it leaves the stream truncated.
pub struct GzipWriter<W: Write> {
    encoder: GzEncoder<W>,
    pending: Vec<u8>,
}

impl<W: Write> GzipWriter<W> {
    pub fn new(inner: W) -> io::Result<Self> {
        Ok(Self {
            encoder: GzEncoder::new(inner, Compression::default()),
            pending: Vec::with_capacity(CHUNK),
        })
    }

    /// End the stream and return the inner writer.
    pub fn finish(mut self) -> io::Result<W> {
        self.encoder.write_all(&self.pending)?;
        self.encoder.finish()
    }
}

impl<W: Write> Write for GzipWriter<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let mut rest = buf;
        while !rest.is_empty() {
            let take = rest.len().min(CHUNK - self.pending.len());
            self.pending.extend_from_slice(&rest[..take]);
            rest = &rest[take..];
            if self.pending.len() == CHUNK {
                self.encoder.write_all(&self.pending)?;
                self.pending.clear();
            }
        }
        Ok(buf.len())
    }

    /// Doesn't reach the compressor: a deflate flush would add a block, and
    /// with it a dependence on when callers flush.
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::io::Read;

    use flate2::read::GzDecoder;

    use super::*;

    fn gzip(chunks: &[&[u8]]) -> Vec<u8> {
        let mut writer = GzipWriter::new(Vec::new()).unwrap();
        for chunk in chunks {
            writer.write_all(chunk).unwrap();
        }
        writer.finish().unwrap()
    }

    /// Text with enough repetition to give deflate matches across chunks.
    fn sample() -> Vec<u8> {
        (0..200_000u32)
            .flat_map(|i| format!("line {} of {}\n", i % 977, i % 13).into_bytes())
            .collect()
    }

    #[test]
    fn round_trips_through_a_gzip_decoder() {
        let data = sample();
        let mut decoded = Vec::new();
        GzDecoder::new(gzip(&[&data]).as_slice())
            .read_to_end(&mut decoded)
            .unwrap();
        assert_eq!(decoded, data);
    }

    #[test]
    fn output_does_not_depend_on_the_input_split() {
        let data = sample();
        let whole = gzip(&[&data]);
        for size in [1, 7, 512, 8191, 100_000, CHUNK + 1] {
            let chunks: Vec<_> = data.chunks(size).collect();
            assert!(gzip(&chunks) == whole, "split into {size}-byte chunks");
        }
    }

    #[test]
    fn empty_input_is_a_valid_stream() {
        let mut decoded = Vec::new();
        GzDecoder::new(gzip(&[]).as_slice())
            .read_to_end(&mut decoded)
            .unwrap();
        assert!(decoded.is_empty());
    }
}
