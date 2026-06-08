use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use crate::error::{ClientError, ErrorCode, Result};
use reddb_wire::redwire::{
    decode_frame, encode_frame, frame_len_from_header, Frame, FRAME_HEADER_SIZE,
};

pub(super) async fn read_frame<S>(stream: &mut S) -> Result<Frame>
where
    S: AsyncRead + Unpin + Send,
{
    let mut header = [0u8; FRAME_HEADER_SIZE];
    stream.read_exact(&mut header).await.map_err(io_err)?;
    let length = frame_len_from_header(&header)
        .map_err(|err| ClientError::new(ErrorCode::Protocol, format!("decode frame: {err}")))?;

    let mut buf = vec![0u8; length];
    buf[..FRAME_HEADER_SIZE].copy_from_slice(&header);
    if length > FRAME_HEADER_SIZE {
        stream
            .read_exact(&mut buf[FRAME_HEADER_SIZE..])
            .await
            .map_err(io_err)?;
    }
    let (frame, _) = decode_frame(&buf)
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
