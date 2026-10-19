//! Cliente HTTP/1.1 sobre hyper con conexiones propias, para medir cada fase (ADR-003).

use std::{
    collections::HashMap,
    error::Error as _,
    io,
    pin::Pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering::Relaxed},
    },
    task::{Context, Poll},
    time::{Duration, Instant},
};

use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::{Request, client::conn::http1::SendRequest};
use hyper_util::rt::TokioIo;
use rustls::pki_types::ServerName;
use serde::Serialize;
use tokio::{
    io::{AsyncRead, AsyncWrite, ReadBuf},
    net::TcpStream,
};
use tokio_rustls::TlsConnector;

pub type Body = Full<Bytes>;
/// Una conexión y el lugar donde anota sus bytes.
type Conn = (SendRequest<Body>, Wire);

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Origin {
    pub https: bool,
    pub host: String,
    pub port: u16,
}

/// Timings de un request. DNS, connect y TLS solo existen si se abrió una conexión nueva.
#[derive(Debug, Default, Clone, Copy)]
pub struct Timings {
    pub dns: Option<Duration>,
    pub connect: Option<Duration>,
    pub tls: Option<Duration>,
    pub ttfb: Duration,
    pub receive: Duration,
}

#[derive(Debug)]
pub struct Response {
    pub status: u16,
    pub headers: hyper::HeaderMap,
    pub content_type: Option<String>,
    pub body: Bytes,
    pub timings: Timings,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FailureKind {
    Dns,
    Connect,
    Tls,
    Timeout,
    Reset,
    Protocol,
    /// No se pudo armar el request (una plantilla sin valor, un header inválido).
    Template,
    /// La respuesta llegó, pero un `extract` no encontró su valor.
    Extract,
}

#[derive(Debug)]
pub struct Failure {
    pub kind: FailureKind,
    pub message: String,
}

impl Failure {
    pub fn new(kind: FailureKind, e: impl std::fmt::Display) -> Self {
        Self {
            kind,
            message: e.to_string(),
        }
    }
}

pub struct Client {
    pools: Mutex<HashMap<Origin, Vec<Conn>>>,
    tls: TlsConnector,
}

impl Default for Client {
    fn default() -> Self {
        Self::new()
    }
}

impl Client {
    pub fn new() -> Self {
        Self {
            pools: Mutex::default(),
            tls: tls_connector(),
        }
    }

    /// Envía un request. `make` se llama de nuevo si una conexión reutilizada
    /// resulta cerrada por el servidor antes de enviar (carrera normal de keep-alive).
    /// Los bytes que pasan por el socket mientras tanto se anotan en `meter`, incluido el
    /// handshake si se abre una conexión nueva.
    pub async fn send(
        &self,
        origin: &Origin,
        make: impl Fn() -> Request<Body>,
        meter: &Arc<Meter>,
    ) -> Result<Response, Failure> {
        if let Some(conn) = self.pooled(origin) {
            conn.1.attach(meter);
            match self
                .exchange(origin, conn, make(), Timings::default())
                .await
            {
                Err(Retry::Stale) => {}
                Err(Retry::No(f)) => return Err(f),
                Ok(r) => return Ok(r),
            }
        }
        let mut timings = Timings::default();
        let conn = self.open(origin, &mut timings, meter).await?;
        self.exchange(origin, conn, make(), timings)
            .await
            .map_err(|e| match e {
                Retry::Stale => {
                    Failure::new(FailureKind::Reset, "la conexión se cerró antes de enviar")
                }
                Retry::No(f) => f,
            })
    }

    fn pooled(&self, origin: &Origin) -> Option<Conn> {
        let mut pools = self.pools.lock().unwrap();
        let pool = pools.get_mut(origin)?;
        while let Some(conn) = pool.pop() {
            if !conn.0.is_closed() {
                return Some(conn);
            }
        }
        None
    }

    async fn exchange(
        &self,
        origin: &Origin,
        (mut conn, wire): Conn,
        request: Request<Body>,
        mut timings: Timings,
    ) -> Result<Response, Retry> {
        if conn.ready().await.is_err() {
            return Err(Retry::Stale);
        }
        let sent = Instant::now();
        let response = conn.send_request(request).await.map_err(|e| {
            if e.is_canceled() {
                Retry::Stale
            } else {
                Retry::No(classify_hyper(&e))
            }
        })?;
        timings.ttfb = sent.elapsed();

        let status = response.status().as_u16();
        let headers = response.headers().clone();
        let content_type = response
            .headers()
            .get(hyper::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .map(String::from);
        let receiving = Instant::now();
        let body = response
            .into_body()
            .collect()
            .await
            .map_err(|e| Retry::No(classify_hyper(&e)))?
            .to_bytes();
        timings.receive = receiving.elapsed();

        if !conn.is_closed() {
            self.pools
                .lock()
                .unwrap()
                .entry(origin.clone())
                .or_default()
                .push((conn, wire));
        }
        Ok(Response {
            status,
            headers,
            content_type,
            body,
            timings,
        })
    }

    async fn open(
        &self,
        origin: &Origin,
        timings: &mut Timings,
        meter: &Arc<Meter>,
    ) -> Result<Conn, Failure> {
        let t = Instant::now();
        let host = origin.host.trim_start_matches('[').trim_end_matches(']');
        let addr = tokio::net::lookup_host((host, origin.port))
            .await
            .map_err(|e| Failure::new(FailureKind::Dns, e))?
            .next()
            .ok_or_else(|| {
                Failure::new(
                    FailureKind::Dns,
                    format!("{host} no resolvió a ninguna dirección"),
                )
            })?;
        timings.dns = Some(t.elapsed());

        let t = Instant::now();
        let tcp = TcpStream::connect(addr)
            .await
            .map_err(|e| Failure::new(classify_io(&e, FailureKind::Connect), e))?;
        tcp.set_nodelay(true).ok();
        timings.connect = Some(t.elapsed());
        // Por debajo de TLS: cuenta lo que pasa por el cable, con el handshake y el cifrado.
        let wire = Wire::default();
        wire.attach(meter);
        let tcp = Counted {
            io: tcp,
            wire: wire.clone(),
        };

        if !origin.https {
            return handshake(tcp, wire).await;
        }
        let t = Instant::now();
        let name = ServerName::try_from(host.to_string())
            .map_err(|e| Failure::new(FailureKind::Tls, e))?;
        let tls = self
            .tls
            .connect(name, tcp)
            .await
            .map_err(|e| Failure::new(FailureKind::Tls, e))?;
        timings.tls = Some(t.elapsed());
        handshake(tls, wire).await
    }
}

/// Bytes de un request: lo que salió y lo que llegó por el socket.
#[derive(Debug, Default)]
pub struct Meter {
    sent: AtomicU64,
    received: AtomicU64,
}

impl Meter {
    pub fn traffic(&self) -> Traffic {
        Traffic {
            sent: self.sent.load(Relaxed),
            received: self.received.load(Relaxed),
        }
    }
}

/// Bytes enviados y recibidos.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Traffic {
    pub sent: u64,
    pub received: u64,
}

/// El medidor del request que está usando la conexión. HTTP/1.1 manda un request a la vez
/// por conexión, así que todo lo que pasa por el socket es suyo.
#[derive(Debug, Clone, Default)]
struct Wire(Arc<Mutex<Arc<Meter>>>);

impl Wire {
    fn attach(&self, meter: &Arc<Meter>) {
        *self.0.lock().unwrap() = meter.clone();
    }

    fn meter(&self) -> Arc<Meter> {
        self.0.lock().unwrap().clone()
    }
}

/// Un socket que anota los bytes que lee y escribe.
struct Counted<T> {
    io: T,
    wire: Wire,
}

impl<T: AsyncRead + Unpin> AsyncRead for Counted<T> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let before = buf.filled().len();
        let poll = Pin::new(&mut self.io).poll_read(cx, buf);
        let n = buf.filled().len() - before;
        if n > 0 {
            self.wire.meter().received.fetch_add(n as u64, Relaxed);
        }
        poll
    }
}

impl<T: AsyncWrite + Unpin> AsyncWrite for Counted<T> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        let poll = Pin::new(&mut self.io).poll_write(cx, buf);
        if let Poll::Ready(Ok(n)) = poll {
            self.wire.meter().sent.fetch_add(n as u64, Relaxed);
        }
        poll
    }

    fn poll_write_vectored(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bufs: &[io::IoSlice<'_>],
    ) -> Poll<io::Result<usize>> {
        let poll = Pin::new(&mut self.io).poll_write_vectored(cx, bufs);
        if let Poll::Ready(Ok(n)) = poll {
            self.wire.meter().sent.fetch_add(n as u64, Relaxed);
        }
        poll
    }

    fn is_write_vectored(&self) -> bool {
        self.io.is_write_vectored()
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.io).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.io).poll_shutdown(cx)
    }
}

enum Retry {
    /// La conexión reutilizada estaba cerrada; el request no salió.
    Stale,
    No(Failure),
}

async fn handshake<T>(io: T, wire: Wire) -> Result<Conn, Failure>
where
    T: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let (sender, connection) = hyper::client::conn::http1::handshake(TokioIo::new(io))
        .await
        .map_err(|e| classify_hyper(&e))?;
    tokio::spawn(async move {
        let _ = connection.await;
    });
    Ok((sender, wire))
}

fn tls_connector() -> TlsConnector {
    let mut roots = rustls::RootCertStore::empty();
    roots.add_parsable_certificates(rustls_native_certs::load_native_certs().certs);
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let mut config = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .expect("ring soporta TLS 1.2 y 1.3")
        .with_root_certificates(roots)
        .with_no_client_auth();
    config.alpn_protocols = vec![b"http/1.1".to_vec()];
    TlsConnector::from(Arc::new(config))
}

fn classify_io(e: &io::Error, default: FailureKind) -> FailureKind {
    match e.kind() {
        io::ErrorKind::TimedOut => FailureKind::Timeout,
        io::ErrorKind::ConnectionReset
        | io::ErrorKind::ConnectionAborted
        | io::ErrorKind::BrokenPipe => FailureKind::Reset,
        io::ErrorKind::ConnectionRefused => FailureKind::Connect,
        _ => default,
    }
}

fn classify_hyper(e: &hyper::Error) -> Failure {
    let mut source = e.source();
    while let Some(s) = source {
        if let Some(io) = s.downcast_ref::<io::Error>() {
            return Failure::new(classify_io(io, FailureKind::Reset), e);
        }
        source = s.source();
    }
    let kind = if e.is_timeout() {
        FailureKind::Timeout
    } else if e.is_incomplete_message() || e.is_closed() || e.is_canceled() {
        FailureKind::Reset
    } else {
        FailureKind::Protocol
    };
    Failure::new(kind, e)
}

/// Copia de un request ya armado, para reintentarlo o reutilizarlo.
pub fn duplicate(request: &Request<Body>) -> Request<Body> {
    let mut copy = Request::builder()
        .method(request.method().clone())
        .uri(request.uri().clone())
        .version(request.version())
        .body(request.body().clone())
        .expect("copia de un request válido");
    *copy.headers_mut() = request.headers().clone();
    copy
}
