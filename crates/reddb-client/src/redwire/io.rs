use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use crate::error::{ClientError, ErrorCode, Result};
use reddb_wire::redwire::{
    decode_frame_parts, encode_frame, frame_len_from_header, Frame, FRAME_HEADER_SIZE,
};

pub(super) async fn read_frame<S>(stream: &mut S) -> Result<Frame>
where
    S: AsyncRead + Unpin + Send,
{
    let mut header = [0u8; FRAME_HEADER_SIZE];
    stream.read_exact(&mut header).await.map_err(io_err)?;
    let length = frame_len_from_header(&header)
        .map_err(|err| ClientError::new(ErrorCode::Protocol, format!("decode frame: {err}")))?;

    let payload_len = length - FRAME_HEADER_SIZE;
    let mut payload = vec![0u8; payload_len];
    if length > FRAME_HEADER_SIZE {
        stream.read_exact(&mut payload).await.map_err(io_err)?;
    }
    let frame = decode_frame_parts(&header, &payload)
        .map_err(|err| ClientError::new(ErrorCode::Protocol, format!("decode frame: {err}")))?;
    Ok(frame)
}

pub(super) async fn write_frame<S>(stream: &mut S, frame: &Frame) -> Result<()>
where
    S: AsyncWrite + Unpin + Send,
{
    stream.write_all(&encode_frame(frame)).await.map_err(io_err)
}

fn io_err(err: std::io::Error) -> ClientError {
    ClientError::new(ErrorCode::Network, err.to_string())
}
