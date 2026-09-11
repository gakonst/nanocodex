//! Exercises real ICE recovery after initial packet loss and slow TCP connection setup.
//! All other runtime behavior, including inbound TCP limits, remains unchanged.

use std::future::Future;
use std::io;
use std::io::IoSliceMut;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::OnceLock;
use std::task::Context;
use std::task::Poll;
use std::time::Duration;
use std::time::Instant;

use futures::future::BoxFuture;
use webrtc::runtime::AsyncInterval;
use webrtc::runtime::AsyncTcpListener;
use webrtc::runtime::AsyncTcpStream;
use webrtc::runtime::AsyncUdpSocket;
use webrtc::runtime::JoinHandle;
use webrtc::runtime::RecvMeta;
use webrtc::runtime::Runtime;
use webrtc::runtime::Transmit;

use crate::transport_runtime::VoiceRuntime;

const INITIAL_OUTAGE: Duration = Duration::from_secs(/*secs*/ 3);

#[derive(Debug, Default)]
pub(super) struct DelayedNetwork {
    inner: VoiceRuntime,
    first_send: Arc<OnceLock<Instant>>,
}

impl Runtime for DelayedNetwork {
    fn spawn(&self, future: BoxFuture<'static, ()>) -> Box<dyn JoinHandle> {
        self.inner.spawn(future)
    }

    fn spawn_reactor(
        &self,
        reactor_pool_size: usize,
        future: BoxFuture<'static, ()>,
    ) -> Box<dyn JoinHandle> {
        self.inner.spawn_reactor(reactor_pool_size, future)
    }

    fn wrap_udp_socket(&self, socket: std::net::UdpSocket) -> io::Result<Arc<dyn AsyncUdpSocket>> {
        Ok(Arc::new(LossyUdp {
            inner: self.inner.wrap_udp_socket(socket)?,
            first_send: self.first_send.clone(),
        }))
    }

    fn wrap_tcp_listener(
        &self,
        listener: std::net::TcpListener,
    ) -> io::Result<Arc<dyn AsyncTcpListener>> {
        self.inner.wrap_tcp_listener(listener)
    }

    fn connect_tcp(
        &self,
        remote_addr: SocketAddr,
    ) -> BoxFuture<'_, io::Result<Arc<dyn AsyncTcpStream>>> {
        Box::pin(async move {
            self.inner.sleep(INITIAL_OUTAGE).await;
            self.inner.connect_tcp(remote_addr).await
        })
    }

    fn resolve_host<'a>(&'a self, host: &'a str) -> BoxFuture<'a, io::Result<Vec<SocketAddr>>> {
        self.inner.resolve_host(host)
    }

    fn sleep(&self, duration: Duration) -> BoxFuture<'static, ()> {
        self.inner.sleep(duration)
    }

    fn interval(&self, period: Duration) -> Box<dyn AsyncInterval> {
        self.inner.interval(period)
    }

    fn block_on(&self, future: Pin<Box<dyn Future<Output = ()> + '_>>) {
        self.inner.block_on(future);
    }

    fn yield_now(&self) -> BoxFuture<'static, ()> {
        self.inner.yield_now()
    }

    fn name(&self) -> &'static str {
        self.inner.name()
    }
}

#[derive(Debug)]
struct LossyUdp {
    inner: Arc<dyn AsyncUdpSocket>,
    first_send: Arc<OnceLock<Instant>>,
}

impl AsyncUdpSocket for LossyUdp {
    fn local_addr(&self) -> io::Result<SocketAddr> {
        self.inner.local_addr()
    }

    fn max_gso_segments(&self) -> usize {
        self.inner.max_gso_segments()
    }

    fn max_gro_segments(&self) -> usize {
        self.inner.max_gro_segments()
    }

    fn poll_send(&self, cx: &mut Context<'_>, transmit: &Transmit<'_>) -> Poll<io::Result<usize>> {
        if self.first_send.get_or_init(Instant::now).elapsed() < INITIAL_OUTAGE {
            // Report success but discard the datagram, like loss after leaving the socket.
            Poll::Ready(Ok(transmit.contents.len()))
        } else {
            self.inner.poll_send(cx, transmit)
        }
    }

    fn poll_recv(
        &self,
        cx: &mut Context<'_>,
        bufs: &mut [IoSliceMut<'_>],
        meta: &mut [RecvMeta],
    ) -> Poll<io::Result<usize>> {
        self.inner.poll_recv(cx, bufs, meta)
    }
}
