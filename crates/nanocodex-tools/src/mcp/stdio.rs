use std::{future::Future, io, process::Stdio, sync::Arc, time::Duration};

use futures_util::{SinkExt, StreamExt};
use rmcp::{
    model::ErrorData,
    service::{RoleClient, RxJsonRpcMessage, TxJsonRpcMessage},
    transport::{
        Transport,
        async_rw::{JsonRpcMessageCodec, JsonRpcMessageCodecError},
    },
};
use tokio::{
    io::AsyncRead,
    process::{Child, ChildStderr, ChildStdin, ChildStdout, Command},
    sync::Mutex,
};
use tokio_util::codec::{FramedRead, FramedWrite};

use crate::shell::ProcessGroupGuard;

const MAX_MCP_STDIO_MESSAGE_BYTES: usize = 8 * 1024 * 1024;
const MCP_STDIO_EXIT_TIMEOUT: Duration = Duration::from_secs(2);

type MessageReader<R> = FramedRead<R, JsonRpcMessageCodec<RxJsonRpcMessage<RoleClient>>>;
type MessageWriter<W> = FramedWrite<W, JsonRpcMessageCodec<TxJsonRpcMessage<RoleClient>>>;

pub(super) struct McpStdioTransport {
    process_group: ProcessGroupGuard,
    child: Option<Child>,
    read: MessageReader<ChildStdout>,
    write: Arc<Mutex<Option<MessageWriter<ChildStdin>>>>,
}

impl McpStdioTransport {
    pub(super) fn spawn(mut command: Command) -> io::Result<(Self, Option<ChildStderr>)> {
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        #[cfg(unix)]
        command.process_group(0);

        let mut child = command.spawn()?;
        let pid = child
            .id()
            .ok_or_else(|| io::Error::other("spawned MCP server without a process identifier"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| io::Error::other("spawned MCP server without stdout"))?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| io::Error::other("spawned MCP server without stdin"))?;
        let stderr = child.stderr.take();
        Ok((
            Self {
                process_group: ProcessGroupGuard::new(pid),
                child: Some(child),
                read: message_reader(stdout, MAX_MCP_STDIO_MESSAGE_BYTES),
                write: Arc::new(Mutex::new(Some(FramedWrite::new(
                    stdin,
                    JsonRpcMessageCodec::new_with_max_length(MAX_MCP_STDIO_MESSAGE_BYTES),
                )))),
            },
            stderr,
        ))
    }
}

fn message_reader<R>(read: R, maximum_bytes: usize) -> MessageReader<R>
where
    R: AsyncRead,
{
    FramedRead::new(
        read,
        JsonRpcMessageCodec::new_with_max_length(maximum_bytes),
    )
}

impl Transport<RoleClient> for McpStdioTransport {
    type Error = io::Error;

    fn send(
        &mut self,
        item: TxJsonRpcMessage<RoleClient>,
    ) -> impl Future<Output = Result<(), Self::Error>> + Send + 'static {
        let write = Arc::clone(&self.write);
        async move {
            let mut write = write.lock().await;
            let write = write
                .as_mut()
                .ok_or_else(|| io::Error::new(io::ErrorKind::NotConnected, "transport closed"))?;
            write.send(item).await.map_err(Into::into)
        }
    }

    async fn receive(&mut self) -> Option<RxJsonRpcMessage<RoleClient>> {
        loop {
            match self.read.next().await {
                Some(Ok(message)) => return Some(message),
                Some(Err(JsonRpcMessageCodecError::Serde(error))) => {
                    tracing::debug!("parse error on incoming MCP stdio message: {error}");
                    let mut write = self.write.lock().await;
                    let write = write.as_mut()?;
                    let response = TxJsonRpcMessage::<RoleClient>::error(
                        ErrorData::parse_error("Parse error", None),
                        None,
                    );
                    if write.send(response).await.is_err() {
                        return None;
                    }
                }
                Some(Err(error)) => {
                    tracing::error!("error reading MCP stdio message: {error}");
                    return None;
                }
                None => return None,
            }
        }
    }

    async fn close(&mut self) -> Result<(), Self::Error> {
        self.write.lock().await.take();

        let terminate = self.process_group.terminate_and_disarm();
        let wait = if let Some(mut child) = self.child.take() {
            match tokio::time::timeout(MCP_STDIO_EXIT_TIMEOUT, child.wait()).await {
                Ok(result) => result.map(|_| ()),
                Err(_) => child.kill().await,
            }
        } else {
            Ok(())
        };

        terminate.and(wait)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::AsyncWriteExt;

    #[tokio::test]
    async fn stdio_message_reader_rejects_an_oversized_frame() {
        let (mut write, read) = tokio::io::duplex(64);
        write.write_all(&[b' '; 33]).await.unwrap();
        write.write_all(b"\n").await.unwrap();
        write.shutdown().await.unwrap();

        let error = message_reader(read, 32).next().await.unwrap().unwrap_err();

        assert!(matches!(
            error,
            JsonRpcMessageCodecError::MaxLineLengthExceeded
        ));
    }
}
