use std::time::Duration;

use crate::{OpenAiAuthSnapshot, ResponsesError, monotonic_now_ns};

use super::{HostConnectRequest, HostConnection, HostError, HostMessage, HostTransport};
use crate::transport::{EncodedRequest, wire::turn_state_from_event};

pub(crate) use crate::transport::wire::{decode_event, parse_raw_json};

const EVENT_IDLE_TIMEOUT: Duration = if cfg!(test) {
    Duration::from_millis(100)
} else {
    Duration::from_mins(5)
};

pub(crate) struct ConnectionMetadata {
    pub status: u16,
    pub request_id: Option<String>,
    pub server_model: Option<String>,
    pub reasoning_included: bool,
    pub turn_state: Option<String>,
}

pub(crate) struct ResponsesSocket {
    connection: Box<dyn HostConnection>,
    turn_state: Option<String>,
}

pub(crate) struct ReceivedText {
    pub text: String,
    pub received_ns: u64,
}

impl ResponsesSocket {
    pub(crate) async fn connect(
        host: &dyn HostTransport,
        endpoint: &str,
        auth: &OpenAiAuthSnapshot,
        session_id: &str,
        thread_id: &str,
        turn_state: Option<&str>,
    ) -> Result<(Self, ConnectionMetadata), ResponsesError> {
        let request = HostConnectRequest::new_with_thread_id(
            endpoint,
            auth.bearer(),
            auth.account_id(),
            auth.is_fedramp(),
            session_id,
            thread_id,
            turn_state,
        );
        let (connection, metadata) = host
            .connect(request)
            .await
            .map_err(|error| match error {
                HostError::HandshakeRejected {
                    status,
                    body,
                    retry_after,
                } => ResponsesError::HandshakeRejected {
                    status,
                    body,
                    retry_after,
                },
                error => ResponsesError::Handshake {
                    reconnectable: error.is_reconnectable(),
                    detail: error.to_string(),
                },
            })?
            .into_parts();
        let metadata = ConnectionMetadata {
            status: metadata.status,
            request_id: metadata.request_id,
            server_model: metadata.server_model,
            reasoning_included: metadata.reasoning_included,
            turn_state: metadata.turn_state,
        };
        Ok((
            Self {
                connection,
                turn_state: metadata.turn_state.clone(),
            },
            metadata,
        ))
    }

    pub(crate) async fn send(&self, request: EncodedRequest) -> Result<(), ResponsesError> {
        self.connection
            .send(request.raw().get())
            .await
            .map_err(|error| {
                let reconnectable = error.is_reconnectable();
                ResponsesError::Send {
                    reconnectable,
                    detail: error.to_string(),
                }
            })
    }

    pub(crate) async fn next_text_or_idle_timeout(
        &mut self,
    ) -> Result<ReceivedText, ResponsesError> {
        match self
            .connection
            .next(EVENT_IDLE_TIMEOUT)
            .await
            .map_err(|error| {
                let reconnectable = error.is_reconnectable();
                ResponsesError::Receive {
                    reconnectable,
                    detail: error.to_string(),
                }
            })? {
            HostMessage::Text(text) => {
                self.capture_turn_state(&text);
                Ok(ReceivedText {
                    text,
                    received_ns: monotonic_now_ns(),
                })
            }
            HostMessage::Closed { detail } => Err(ResponsesError::Closed { detail }),
            HostMessage::Timeout => Err(ResponsesError::IdleTimeout {
                seconds: EVENT_IDLE_TIMEOUT.as_secs(),
            }),
            HostMessage::Binary => Err(ResponsesError::UnexpectedBinary),
        }
    }

    pub(crate) fn turn_state(&self) -> Option<&str> {
        self.turn_state.as_deref()
    }

    pub(crate) fn reset_turn_state(&mut self) {
        self.turn_state = None;
    }

    fn capture_turn_state(&mut self, text: &str) {
        if self.turn_state.is_none() {
            self.turn_state = turn_state_from_event(text);
        }
    }
}

impl Drop for ResponsesSocket {
    fn drop(&mut self) {
        self.connection.close();
    }
}
