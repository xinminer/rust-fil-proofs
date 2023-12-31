use std::cmp::min;
use std::io::{self, Read};

use anyhow::{ensure, Result};
use filecoin_hashers::{HashFunction, Hasher};
use rayon::prelude::{ParallelIterator, ParallelSlice};

use crate::{constants::DefaultPieceHasher, pieces::piece_hash};

const BUFFER_SIZE: usize = 4096;

/// Calculates comm-d of the data piped through to it.
/// Data must be bit padded and power of 2 bytes.
pub struct CommitmentReader<R> {
    source: R,
    buffer: Vec<u8>,
    buffer_pos: usize,
    current_tree: Vec<<DefaultPieceHasher as Hasher>::Domain>,
}

impl<R: Read> CommitmentReader<R> {
    pub fn new(source: R) -> Self {
        CommitmentReader {
            source,
            buffer: vec![0u8; BUFFER_SIZE],
            buffer_pos: 0,
            current_tree: Vec::new(),
        }
    }

    /// Attempt to generate the next hash, but only if the buffers are full.
    fn try_hash(&mut self) {
        // Get more bytes in case we cannot iterate cleanly in 64 byte chunks.
        if self.buffer_pos % 64 != 0 {
            return;
        }

        for chunk in self.buffer[..self.buffer_pos].chunks_exact(64) {
            // WARNING: keep in sync with DefaultPieceHasher and its .node impl
            let hash = <DefaultPieceHasher as Hasher>::Function::hash(chunk);
            self.current_tree.push(hash);
        }
        self.buffer_pos = 0;

        // TODO: reduce hashes when possible, instead of keeping them around.
    }

    pub fn finish(self) -> Result<<DefaultPieceHasher as Hasher>::Domain> {
        ensure!(self.buffer_pos == 0, "not enough inputs provided");

        let CommitmentReader { current_tree, .. } = self;

        let mut current_row = current_tree;

        while current_row.len() > 1 {
            let next_row = current_row
                .par_chunks(2)
                .map(|chunk| piece_hash(chunk[0].as_ref(), chunk[1].as_ref()))
                .collect::<Vec<_>>();

            current_row = next_row;
        }
        debug_assert_eq!(current_row.len(), 1);

        Ok(current_row
            .into_iter()
            .next()
            .expect("should have been caught by debug build: len==1"))
    }
}

impl<R: Read> Read for CommitmentReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let start = self.buffer_pos;
        let left = BUFFER_SIZE - self.buffer_pos;
        let end = start + min(left, buf.len());

        // fill the buffer as much as possible
        let r = self.source.read(&mut self.buffer[start..end])?;

        // write the data, we read
        buf[..r].copy_from_slice(&self.buffer[start..start + r]);

        self.buffer_pos += r;

        // try to hash
        self.try_hash();

        Ok(r)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::io::Cursor;

    use fr32::Fr32Reader;
    use storage_proofs_core::pieces::generate_piece_commitment_bytes_from_source;

    use crate::types::{PaddedBytesAmount, UnpaddedBytesAmount};

    #[test]
    fn test_commitment_reader() {
        let piece_size = 127 * 8;
        let source = vec![255u8; piece_size];
        let mut fr32_reader = Fr32Reader::new(Cursor::new(&source));

        let commitment1 = generate_piece_commitment_bytes_from_source::<DefaultPieceHasher>(
            &mut fr32_reader,
            PaddedBytesAmount::from(UnpaddedBytesAmount(piece_size as u64)).into(),
        )
        .expect("failed to generate piece commitment bytes from source");

        let fr32_reader = Fr32Reader::new(Cursor::new(&source));
        let mut commitment_reader = CommitmentReader::new(fr32_reader);
        io::copy(&mut commitment_reader, &mut io::sink()).expect("io copy failed");

        let commitment2 = commitment_reader.finish().expect("failed to finish");

        assert_eq!(&commitment1[..], AsRef::<[u8]>::as_ref(&commitment2));
    }
}
