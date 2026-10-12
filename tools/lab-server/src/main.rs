//! Servidor de laboratorio con comportamientos conocidos, para probar el motor.
//!
//! Rutas:
//! - `GET /ok`          responde al instante.
//! - `GET /delay/{ms}`  responde después de `ms` milisegundos.
//! - `GET /pool/{ms}`   como `/delay`, pero solo `--pool` requests a la vez;
//!                      el resto espera. Capacidad: pool × 1000 / ms req/s.

use std::{sync::Arc, time::Duration};

use axum::{Router, extract::Path, extract::State, routing::get};
use clap::Parser;
use tokio::{net::TcpListener, sync::Semaphore, time::sleep};

#[derive(Parser)]
struct Args {
    #[arg(long, default_value_t = 3000)]
    port: u16,
    /// Requests simultáneos que atiende `/pool`.
    #[arg(long, default_value_t = 4)]
    pool: usize,
}

#[tokio::main]
async fn main() {
    high_resolution_timer();
    let args = Args::parse();
    let pool = Arc::new(Semaphore::new(args.pool));

    let app = Router::new()
        .route("/ok", get(|| async { "ok" }))
        .route("/delay/{ms}", get(delay))
        .route("/pool/{ms}", get(pooled))
        .with_state(pool);

    let listener = TcpListener::bind(("127.0.0.1", args.port)).await.unwrap();
    println!(
        "lab-server en http://127.0.0.1:{} (pool={})",
        args.port, args.pool
    );
    axum::serve(listener, app).await.unwrap();
}

/// En Windows el timer tiene ~15,6 ms de resolución por defecto y todo `sleep`
/// se redondea hacia arriba. Pedimos 1 ms para todo el proceso.
fn high_resolution_timer() {
    #[cfg(windows)]
    unsafe {
        windows_sys::Win32::Media::timeBeginPeriod(1);
    }
}

async fn delay(Path(ms): Path<u64>) -> &'static str {
    sleep(Duration::from_millis(ms)).await;
    "ok"
}

async fn pooled(State(pool): State<Arc<Semaphore>>, Path(ms): Path<u64>) -> &'static str {
    let _permit = pool.acquire().await.unwrap();
    sleep(Duration::from_millis(ms)).await;
    "ok"
}
