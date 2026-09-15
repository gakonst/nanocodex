//! Exercise real frame decoding across pipe boundaries, cancellation, and invalid input.

use std::future::Future;
use std::io;
use std::task::Context;
use std::task::Poll;
use std::task::Waker;

use pretty_assertions::assert_eq;
use tokio::sync::mpsc;

use super::MAX_CHUNK_BYTES;
use super::MessageReader;
use crate::MAX_FRAME_BYTES;
use crate::Message;
use crate::encode_frame;

#[tokio::test]
async fn reads_fragmented_and_coalesced_messages() -> io::Result<()> {
    let expected = [
        Message::Hello {
            protocol: 1,
            build_commit: "test-build".into(),
        },
        Message::Ready {},
        Message::Close {},
        Message::Closed {},
    ];
    let frames = expected
        .iter()
        .map(encode_frame)
        .collect::<io::Result<Vec<_>>>()?
        .concat();
    // Every split exercises both a partial frame and multiple messages in a chunk.
    for split in 1..frames.len() {
        let (sender, receiver) = mpsc::channel(/*buffer*/ 2);
        sender.try_send(frames[..split].to_vec()).unwrap();
        sender.try_send(frames[split..].to_vec()).unwrap();
        drop(sender);
        let mut reader = MessageReader::new(receiver);
        let mut actual = Vec::new();
        for _ in &expected {
            actual.push(reader.next().await?);
        }
        assert_eq!(actual, expected);
        assert_eq!(
            reader.next().await.unwrap_err().kind(),
            io::ErrorKind::UnexpectedEof
        );
    }
    Ok(())
}

#[tokio::test]
async fn cancelled_read_preserves_each_partial_header_and_payload() -> io::Result<()> {
    let expected = Message::Hello {
        protocol: 1,
        build_commit: "test-build".into(),
    };
    let frame = encode_frame(&expected)?;
    for split in 1..frame.len() {
        let (sender, receiver) = mpsc::channel(/*buffer*/ 1);
        sender.try_send(frame[..split].to_vec()).unwrap();
        let mut reader = MessageReader::new(receiver);
        {
            let mut read = std::pin::pin!(reader.next());
            assert!(matches!(
                read.as_mut().poll(&mut Context::from_waker(Waker::noop())),
                Poll::Pending
            ));
        }
        sender.try_send(frame[split..].to_vec()).unwrap();
        assert_eq!(reader.next().await?, expected);
    }
    Ok(())
}

#[tokio::test]
async fn rejects_oversized_headers_without_waiting_for_payload() {
    for length in [MAX_FRAME_BYTES as u32 + 1, u32::MAX] {
        let (sender, receiver) = mpsc::channel(/*buffer*/ 1);
        sender.try_send(length.to_be_bytes().to_vec()).unwrap();
        let mut reader = MessageReader::new(receiver);
        let mut read = std::pin::pin!(reader.next());
        assert!(matches!(
            read.as_mut().poll(&mut Context::from_waker(Waker::noop())),
            Poll::Ready(Err(_))
        ));
    }
}

#[tokio::test]
async fn rejects_truncation_at_every_byte_boundary() -> io::Result<()> {
    let frame = encode_frame(&Message::Ready {})?;
    for end in 0..frame.len() {
        let (sender, receiver) = mpsc::channel(/*buffer*/ 1);
        if end > 0 {
            sender.try_send(frame[..end].to_vec()).unwrap();
        }
        drop(sender);
        assert_eq!(
            MessageReader::new(receiver)
                .next()
                .await
                .unwrap_err()
                .kind(),
            io::ErrorKind::UnexpectedEof
        );
    }
    Ok(())
}

#[tokio::test]
async fn rejects_malformed_input_without_echoing_payload() {
    let secret = b"sensitive-invalid-json";
    let mut invalid = (secret.len() as u32).to_be_bytes().to_vec();
    invalid.extend(secret);
    let (sender, receiver) = mpsc::channel(/*buffer*/ 1);
    sender.try_send(invalid).unwrap();
    assert_eq!(
        MessageReader::new(receiver)
            .next()
            .await
            .unwrap_err()
            .to_string(),
        "invalid voice frame"
    );
}

#[tokio::test]
async fn rejects_chunks_outside_the_pipe_read_bound() {
    for chunk in [vec![], vec![0; MAX_CHUNK_BYTES + 1]] {
        let (sender, receiver) = mpsc::channel(/*buffer*/ 1);
        sender.try_send(chunk).unwrap();
        assert_eq!(
            MessageReader::new(receiver)
                .next()
                .await
                .unwrap_err()
                .to_string(),
            "invalid voice helper output chunk"
        );
    }
}
