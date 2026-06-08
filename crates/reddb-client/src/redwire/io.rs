use tokio::io::{AsyncRead, AsyncWrite};

use crate::error::{ClientError, ErrorCode, Result};
use reddb_wire::redwire::{read_frame_async, write_frame_async, Frame, RedWireIoError};

pub(super) async fn read_frame<S>(stream: &mut S) -> Result<Frame>
where
    S: AsyncRead + Unpin + Send,
{
    read_frame_async(stream).await.map_err(client_io_err)
}

pub(super) async fn write_frame<S>(stream: &mut S, frame: &Frame) -> Result<()>
where
    S: AsyncWrite + Unpin + Send,
{
    write_frame_async(stream, frame)
        .await
        .map_err(client_io_err)
}

fn client_io_err(err: RedWireIoError) -> ClientError {
    match err {
        RedWireIoError::Io(err) => ClientError::new(ErrorCode::Network, err.to_string()),
        RedWireIoError::Frame(err) => {
            ClientError::new(ErrorCode::Protocol, format!("decode frame: {err}"))
        }
    }
}
