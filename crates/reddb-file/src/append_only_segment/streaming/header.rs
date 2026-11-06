//! A bounded pipe lets serde resume across ticks without buffering the header.
//! Only validation metadata is retained; bloom/key strings are parsed as ignored
//! JSON. Closing the pipe cancels an unfinished parser, including on cold boot.
use std::io::{self, Read};
use std::sync::mpsc::{self, Receiver, SyncSender};
use std::thread::{self, JoinHandle};

use super::{invalid, AppendOnlySegmentChunkChecksum, AppendOnlySegmentCodec, SegmentHeader};

#[derive(serde::Deserialize)]
struct ValidationHeader {
    version: u32,
    codec: AppendOnlySegmentCodec,
    chunk_size: u32,
    row_count: u64,
    uncompressed_len: u64,
    chunk_checksums: Vec<AppendOnlySegmentChunkChecksum>,
}

struct Pipe {
    receiver: Receiver<Vec<u8>>,
    bytes: std::io::Cursor<Vec<u8>>,
}

impl Read for Pipe {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        if out.is_empty() {
            return Ok(0);
        }
        loop {
            let read = self.bytes.read(out)?;
            if read != 0 {
                return Ok(read);
            }
            match self.receiver.recv() {
                Ok(bytes) => self.bytes = std::io::Cursor::new(bytes),
                Err(_) => return Ok(0),
            }
        }
    }
}

pub(super) struct HeaderDecoder {
    sender: Option<SyncSender<Vec<u8>>>,
    worker: Option<JoinHandle<io::Result<SegmentHeader>>>,
}

impl HeaderDecoder {
    pub fn new() -> io::Result<Self> {
        let (sender, receiver) = mpsc::sync_channel(1);
        let worker = thread::Builder::new()
            .name("segment-header".into())
            .spawn(move || {
                let pipe = Pipe {
                    receiver,
                    bytes: std::io::Cursor::new(Vec::new()),
                };
                let header: ValidationHeader =
                    serde_json::from_reader(pipe).map_err(|error| invalid(error.to_string()))?;
                Ok(SegmentHeader {
                    version: header.version,
                    codec: header.codec,
                    chunk_size: header.chunk_size,
                    row_count: header.row_count,
                    uncompressed_len: header.uncompressed_len,
                    primary_min_hex: None,
                    primary_max_hex: None,
                    primary_bloom: None,
                    chunk_checksums: header.chunk_checksums,
                })
            })?;
        Ok(Self {
            sender: Some(sender),
            worker: Some(worker),
        })
    }

    pub fn push(&self, bytes: Vec<u8>) -> io::Result<()> {
        self.sender
            .as_ref()
            .expect("decoder sender exists")
            .send(bytes)
            .map_err(|_| invalid("invalid streaming segment header"))
    }

    pub fn finish(mut self) -> io::Result<SegmentHeader> {
        self.sender.take();
        self.worker
            .take()
            .expect("decoder worker exists")
            .join()
            .map_err(|_| io::Error::other("segment header parser panicked"))?
    }
}

impl Drop for HeaderDecoder {
    fn drop(&mut self) {
        self.sender.take();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}
