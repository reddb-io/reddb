use futures_util::{SinkExt, StreamExt};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_tungstenite::tungstenite::{client::IntoClientRequest, Message};

use super::Stream;
use crate::error::{ClientError, ErrorCode, Result};

/// Bound the bridge and reuse the identical RedWire handshake and codecs.
pub(super) async fn connect(url: &str, options: &super::ConnectOptions) -> Result<Stream> {
    let mut request = url.into_client_request().map_err(network_error)?;
    request.headers_mut().insert(
        "sec-websocket-protocol",
        reddb_wire::redwire::REDWIRE_WS_SUBPROTOCOL
            .parse()
            .expect("invariant: RedWire WebSocket subprotocol is an HTTP header"),
    );
    let tcp = tokio::net::TcpStream::connect((options.host.as_str(), options.port))
        .await
        .map_err(network_error)?;
    let stream: Stream = match options.tls.as_ref() {
        Some(tls) => Box::pin(super::tls::wrap_client(tcp, &options.host, tls).await?),
        None => Box::pin(tcp),
    };
    let (socket, _) = tokio_tungstenite::client_async(request, stream)
        .await
        .map_err(network_error)?;
    let (client, bridge) = tokio::io::duplex(64 * 1024);
    let (mut input, mut output) = tokio::io::split(bridge);
    let (mut sink, mut source) = socket.split();
    tokio::spawn(async move {
        let outgoing = async {
            let mut bytes = vec![0; 16 * 1024];
            loop {
                match input.read(&mut bytes).await {
                    Ok(0) | Err(_) => break,
                    Ok(count) => {
                        if sink
                            .send(Message::Binary(bytes[..count].to_vec().into()))
                            .await
                            .is_err()
                        {
                            break;
                        }
                    }
                }
            }
            let _ = sink.close().await;
        };
        let incoming = async {
            while let Some(Ok(message)) = source.next().await {
                match message {
                    Message::Binary(bytes) if output.write_all(&bytes).await.is_err() => break,
                    Message::Close(_) => break,
                    _ => {}
                }
            }
            let _ = output.shutdown().await;
        };
        tokio::select! { _ = outgoing => {}, _ = incoming => {} }
    });
    Ok(Box::pin(client))
}

fn network_error(error: impl std::fmt::Display) -> ClientError {
    ClientError::new(ErrorCode::Network, error.to_string())
}
