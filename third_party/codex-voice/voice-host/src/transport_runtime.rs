//! Bounds retained inbound TCP streams across one peer's listeners before authentication.
//! Weak references reuse slots after stream ownership ends; other Tokio I/O stays unchanged.

use std::future::Future;
use std::io;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::Weak;
use std::time::Duration;

use futures::future::BoxFuture;
use webrtc::runtime::AsyncInterval;
use webrtc::runtime::AsyncTcpListener;
use webrtc::runtime::AsyncTcpStream;
use webrtc::runtime::AsyncUdpSocket;
use webrtc::runtime::JoinHandle;
use webrtc::runtime::Runtime;
use webrtc::runtime::TokioRuntime;

const MAX_INBOUND_STREAMS: usize = 32;
type AcceptedStreams = Arc<Mutex<Vec<Weak<dyn AsyncTcpStream>>>>;

#[derive(Debug, Default)]
pub(crate) struct VoiceRuntime {
    accepted: AcceptedStreams,
}

impl Runtime for VoiceRuntime {
    fn spawn(&self, future: BoxFuture<'static, ()>) -> Box<dyn JoinHandle> {
        TokioRuntime.spawn(future)
    }

    fn spawn_reactor(
        &self,
        reactor_pool_size: usize,
        future: BoxFuture<'static, ()>,
    ) -> Box<dyn JoinHandle> {
        TokioRuntime.spawn_reactor(reactor_pool_size, future)
    }

    fn wrap_udp_socket(&self, socket: std::net::UdpSocket) -> io::Result<Arc<dyn AsyncUdpSocket>> {
        TokioRuntime.wrap_udp_socket(socket)
    }

    fn wrap_tcp_listener(
        &self,
        listener: std::net::TcpListener,
    ) -> io::Result<Arc<dyn AsyncTcpListener>> {
        Ok(Arc::new(LimitedListener {
            inner: TokioRuntime.wrap_tcp_listener(listener)?,
            accepted: self.accepted.clone(),
        }))
    }

    fn connect_tcp(
        &self,
        remote_addr: SocketAddr,
    ) -> BoxFuture<'_, io::Result<Arc<dyn AsyncTcpStream>>> {
        TokioRuntime.connect_tcp(remote_addr)
    }

    fn resolve_host<'a>(&'a self, host: &'a str) -> BoxFuture<'a, io::Result<Vec<SocketAddr>>> {
        TokioRuntime.resolve_host(host)
    }

    fn sleep(&self, duration: Duration) -> BoxFuture<'static, ()> {
        TokioRuntime.sleep(duration)
    }

    fn interval(&self, period: Duration) -> Box<dyn AsyncInterval> {
        TokioRuntime.interval(period)
    }

    fn block_on(&self, future: Pin<Box<dyn Future<Output = ()> + '_>>) {
        TokioRuntime.block_on(future);
    }

    fn yield_now(&self) -> BoxFuture<'static, ()> {
        TokioRuntime.yield_now()
    }

    fn name(&self) -> &'static str {
        TokioRuntime.name()
    }
}

#[derive(Debug)]
struct LimitedListener {
    inner: Arc<dyn AsyncTcpListener>,
    accepted: AcceptedStreams,
}

impl AsyncTcpListener for LimitedListener {
    fn accept(&self) -> BoxFuture<'_, io::Result<(Arc<dyn AsyncTcpStream>, SocketAddr)>> {
        Box::pin(async move {
            let (stream, address) = self.inner.accept().await?;
            let mut accepted = self
                .accepted
                .lock()
                .map_err(|_| io::Error::other("voice TCP admission unavailable"))?;
            accepted.retain(|stream| stream.strong_count() > 0);
            if accepted.len() >= MAX_INBOUND_STREAMS {
                return Err(io::Error::other("voice TCP connection limit reached"));
            }
            accepted.push(Arc::downgrade(&stream));
            Ok((stream, address))
        })
    }

    fn local_addr(&self) -> io::Result<SocketAddr> {
        self.inner.local_addr()
    }
}

#[cfg(test)]
#[path = "transport_runtime_tests.rs"]
mod tests;
