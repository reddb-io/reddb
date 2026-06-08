//! Async RedWire frame I/O over any Tokio byte stream.
//!
//! This module owns the read/write choreography around the canonical frame
//! codec. Transport setup, authentication policy, and dispatch remain outside
//! `reddb-wire`.

use std::io;

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use super::{
    decode_frame_parts, encode_frame, frame_len_from_header, Frame, FrameError, FRAME_HEADER_SIZE,
};

#[derive(Debug)]
pub enum RedWireIoError {
    Io(io::Error),
    Frame(FrameError),
}

impl std::fmt::Display for RedWireIoError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(err) => write!(f, "{err}"),
            Self::Frame(err) => write!(f, "{err}"),
        }
    }
}

impl std::error::Error for RedWireIoError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(err) => Some(err),
            Self::Frame(err) => Some(err),
        }
    }
}

impl From<io::Error> for RedWireIoError {
    fn from(err: io::Error) -> Self {
        Self::Io(err)
    }
}

impl From<FrameError> for RedWireIoError {
    fn from(err: FrameError) -> Self {
        Self::Frame(err)
    }
}

pub async fn read_frame_async<S>(stream: &mut S) -> Result<Frame, RedWireIoError>
where
    S: AsyncRead + Unpin + Send,
{
    let mut header = [0u8; FRAME_HEADER_SIZE];
    stream.read_exact(&mut header).await?;
    let length = frame_len_from_header(&header)?;

    let payload_len = length - FRAME_HEADER_SIZE;
    let mut payload = vec![0u8; payload_len];
    if length > FRAME_HEADER_SIZE {
        stream.read_exact(&mut payload).await?;
    }
    Ok(decode_frame_parts(&header, &payload)?)
}

pub async fn write_frame_async<S>(stream: &mut S, frame: &Frame) -> Result<(), RedWireIoError>
where
    S: AsyncWrite + Unpin + Send,
{
    stream.write_all(&encode_frame(frame)).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::redwire::MessageKind;

    #[tokio::test]
    async fn async_frame_io_round_trips_over_duplex() {
        let (mut left, mut right) = tokio::io::duplex(1024);
        let frame = Frame::new(MessageKind::Ping, 42, b"hello".to_vec());

        write_frame_async(&mut left, &frame).await.unwrap();
        let decoded = read_frame_async(&mut right).await.unwrap();

        assert_eq!(decoded, frame);
    }
}
