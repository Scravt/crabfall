//! Spike descartable (rebanada 0): scheduler de tasa constante sobre hyper.
//!
//! Preguntas que responde:
//! - ¿Se puede medir DNS, connect, TTFB y receiving por separado con hyper? (ADR-003)
//! - ¿El scheduler mantiene la tasa aunque el servidor se frene?
//! - ¿Medir desde el instante planificado evita la coordinated omission?
//!
//! Solo HTTP/1.1 sin TLS.

use std::{
    error::Error,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use bytes::Bytes;
use clap::Parser;
use hdrhistogram::Histogram;
use http_body_util::{BodyExt, Empty};
use hyper::{Request, Uri, client::conn::http1::SendRequest, header::HOST};
use hyper_util::rt::TokioIo;
use tokio::{
    net::TcpStream,
    sync::{Semaphore, mpsc},
    time::{sleep_until, timeout},
};

type BoxError = Box<dyn Error + Send + Sync>;
type Conn = SendRequest<Empty<Bytes>>;

#[derive(Parser)]
struct Args {
    /// URL destino, por ejemplo http://127.0.0.1:3000/delay/5
    url: Uri,
    /// Requests por segundo.
    #[arg(long, default_value_t = 100)]
    rate: u32,
    /// Total de requests a planificar.
    #[arg(long, default_value_t = 1000)]
    requests: u32,
    /// Máximo de requests en vuelo. Si no hay lugar, la iteración se descarta.
    #[arg(long, default_value_t = 100)]
    max_vus: usize,
    /// Timeout por request, en segundos.
    #[arg(long, default_value_t = 10)]
    timeout: u64,
}

struct Target {
    host: String,
    port: u16,
    authority: String,
    path: String,
}

/// Timings de un request completado. `intended` es el instante planificado.
struct Sample {
    intended: Instant,
    dispatched: Instant,
    done: Instant,
    dns: Option<Duration>,
    connect: Option<Duration>,
    ttfb: Duration,
    receive: Duration,
    success: bool,
}

enum Outcome {
    Done(Sample),
    Failed(String),
    Dropped,
}

#[tokio::main]
async fn main() {
    high_resolution_timer();
    let args = Args::parse();
    let target = Arc::new(Target {
        host: args.url.host().expect("la URL necesita host").to_string(),
        port: args.url.port_u16().unwrap_or(80),
        authority: args.url.authority().unwrap().to_string(),
        path: args
            .url
            .path_and_query()
            .map_or("/".into(), |p| p.to_string()),
    });

    let pool: Arc<Mutex<Vec<Conn>>> = Arc::default();
    let vus = Arc::new(Semaphore::new(args.max_vus));
    let request_timeout = Duration::from_secs(args.timeout);
    let (tx, mut rx) = mpsc::unbounded_channel();

    let collector = tokio::spawn(async move {
        let mut stats = Stats::new();
        while let Some(outcome) = rx.recv().await {
            stats.record(outcome);
        }
        stats
    });

    let interval = Duration::from_secs_f64(1.0 / f64::from(args.rate));
    let start = Instant::now() + Duration::from_millis(50);
    let mut lag = histogram();
    let mut last_dispatch = start;

    for i in 0..args.requests {
        let intended = start + interval * i;
        sleep_until(intended.into()).await;
        last_dispatch = Instant::now();
        record(&mut lag, last_dispatch - intended);

        let Ok(permit) = vus.clone().try_acquire_owned() else {
            tx.send(Outcome::Dropped).unwrap();
            continue;
        };
        let (target, pool, tx) = (target.clone(), pool.clone(), tx.clone());
        tokio::spawn(async move {
            let outcome = match timeout(request_timeout, execute(&target, &pool, intended)).await {
                Ok(Ok(sample)) => Outcome::Done(sample),
                Ok(Err(e)) => Outcome::Failed(e.to_string()),
                Err(_) => Outcome::Failed("timeout".into()),
            };
            drop(permit);
            tx.send(outcome).unwrap();
        });
    }
    drop(tx);

    let stats = collector.await.unwrap();
    let schedule_secs = (last_dispatch - start).as_secs_f64();
    let total_secs = start.elapsed().as_secs_f64();
    stats.print(args.rate, schedule_secs, total_secs, &lag);
}

async fn execute(
    target: &Target,
    pool: &Mutex<Vec<Conn>>,
    intended: Instant,
) -> Result<Sample, BoxError> {
    let dispatched = Instant::now();

    let pooled = pool.lock().unwrap().pop();
    let (mut conn, dns, connect) = match pooled {
        Some(conn) if !conn.is_closed() => (conn, None, None),
        _ => {
            let (conn, dns, connect) = open(target).await?;
            (conn, Some(dns), Some(connect))
        }
    };

    conn.ready().await?;
    let request = Request::get(target.path.as_str())
        .header(HOST, target.authority.as_str())
        .body(Empty::new())?;

    let sent = Instant::now();
    let response = conn.send_request(request).await?;
    let ttfb = sent.elapsed();

    let success = response.status().is_success();
    let receiving = Instant::now();
    response.into_body().collect().await?;
    let receive = receiving.elapsed();
    let done = Instant::now();

    if !conn.is_closed() {
        pool.lock().unwrap().push(conn);
    }

    Ok(Sample {
        intended,
        dispatched,
        done,
        dns,
        connect,
        ttfb,
        receive,
        success,
    })
}

/// Abre una conexión nueva midiendo DNS y connect por separado.
async fn open(target: &Target) -> Result<(Conn, Duration, Duration), BoxError> {
    let t = Instant::now();
    let addr = tokio::net::lookup_host((target.host.as_str(), target.port))
        .await?
        .next()
        .ok_or("el host no resolvió a ninguna dirección")?;
    let dns = t.elapsed();

    let t = Instant::now();
    let stream = TcpStream::connect(addr).await?;
    stream.set_nodelay(true)?;
    let connect = t.elapsed();

    let (sender, connection) = hyper::client::conn::http1::handshake(TokioIo::new(stream)).await?;
    tokio::spawn(async move {
        let _ = connection.await;
    });
    Ok((sender, dns, connect))
}

/// En Windows el timer tiene ~15,6 ms de resolución por defecto y todo `sleep`
/// se redondea hacia arriba. Pedimos 1 ms para todo el proceso.
fn high_resolution_timer() {
    #[cfg(windows)]
    unsafe {
        windows_sys::Win32::Media::timeBeginPeriod(1);
    }
}

fn histogram() -> Histogram<u64> {
    // Microsegundos, de 1 µs a 60 s, 3 cifras significativas.
    Histogram::new_with_bounds(1, 60_000_000, 3).unwrap()
}

fn record(h: &mut Histogram<u64>, d: Duration) {
    h.saturating_record(d.as_micros().max(1) as u64);
}

struct Stats {
    completed: u64,
    non_2xx: u64,
    failed: u64,
    dropped: u64,
    errors: Vec<String>,
    latency: Histogram<u64>,
    service: Histogram<u64>,
    ttfb: Histogram<u64>,
    receive: Histogram<u64>,
    dns: Histogram<u64>,
    connect: Histogram<u64>,
}

impl Stats {
    fn new() -> Self {
        Self {
            completed: 0,
            non_2xx: 0,
            failed: 0,
            dropped: 0,
            errors: Vec::new(),
            latency: histogram(),
            service: histogram(),
            ttfb: histogram(),
            receive: histogram(),
            dns: histogram(),
            connect: histogram(),
        }
    }

    fn record(&mut self, outcome: Outcome) {
        match outcome {
            Outcome::Done(s) => {
                self.completed += 1;
                if !s.success {
                    self.non_2xx += 1;
                }
                record(&mut self.latency, s.done - s.intended);
                record(&mut self.service, s.done - s.dispatched);
                record(&mut self.ttfb, s.ttfb);
                record(&mut self.receive, s.receive);
                if let Some(d) = s.dns {
                    record(&mut self.dns, d);
                }
                if let Some(c) = s.connect {
                    record(&mut self.connect, c);
                }
            }
            Outcome::Failed(e) => {
                self.failed += 1;
                if self.errors.len() < 5 && !self.errors.contains(&e) {
                    self.errors.push(e);
                }
            }
            Outcome::Dropped => self.dropped += 1,
        }
    }

    fn print(&self, rate: u32, schedule_secs: f64, total_secs: f64, lag: &Histogram<u64>) {
        let planned = self.completed + self.failed + self.dropped;
        let sent = self.completed + self.failed;
        println!(
            "planificados {planned}  enviados {sent}  completados {}  fallidos {}  no-2xx {}  descartados {}",
            self.completed, self.failed, self.non_2xx, self.dropped
        );
        println!(
            "tasa pedida {rate}/s  tasa de envío {:.1}/s  duración {total_secs:.2}s",
            sent as f64 / schedule_secs.max(1e-9)
        );
        println!();
        println!(
            "{:<26}{:>9}{:>9}{:>9}{:>9}{:>9}{:>9}{:>9}",
            "(ms)", "n", "p50", "p90", "p95", "p99", "p99.9", "max"
        );
        row("latencia (desde planif.)", &self.latency);
        row("servicio (desde envío)", &self.service);
        row("ttfb", &self.ttfb);
        row("receiving", &self.receive);
        row("connect (conex. nuevas)", &self.connect);
        row("dns (conex. nuevas)", &self.dns);
        row("lag del scheduler", lag);

        for e in &self.errors {
            println!("error: {e}");
        }
        if self.dropped > 0 {
            println!(
                "AVISO: {} iteraciones descartadas por falta de VUs: el servidor no da abasto o max_vus es bajo.",
                self.dropped
            );
        }
        if lag.value_at_quantile(0.99) > 5_000 {
            println!(
                "AVISO: el scheduler llega tarde (p99 > 5 ms): el generador puede ser el cuello de botella."
            );
        }
    }
}

fn row(name: &str, h: &Histogram<u64>) {
    let ms = |q: f64| h.value_at_quantile(q) as f64 / 1000.0;
    if h.is_empty() {
        println!("{name:<26}{:>9}", 0);
        return;
    }
    println!(
        "{name:<26}{:>9}{:>9.2}{:>9.2}{:>9.2}{:>9.2}{:>9.2}{:>9.2}",
        h.len(),
        ms(0.5),
        ms(0.9),
        ms(0.95),
        ms(0.99),
        ms(0.999),
        h.max() as f64 / 1000.0
    );
}
