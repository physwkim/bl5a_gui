//! `bl5a-qs` — the bsrs queue server for BL5A: the coin-cell and
//! thin-film macros (coin_align, mymar1/mymar, align_in.mac, mymar_in) as
//! three plans over spec-ioc and mar345-ioc.
//!
//! ```sh
//! cargo run -p bl5a-qs -- --data-root /data/bl5a
//! bsrs qs --address tcp://localhost:60715 allowed plans
//! ```

use std::fmt;
use std::path::PathBuf;
use std::sync::Arc;

use bsrs::qs::{Registry, Server};
use clap::Parser;
use tokio::signal::unix::{SignalKind, signal};

mod ctx;
mod devices;
mod peak;
mod place;
mod plans;
mod scan;
mod screen;
mod specfile;

use devices::Devices;

/// The pause between start-up attempts to connect every PV.
const CONNECT_RETRY: std::time::Duration = std::time::Duration::from_secs(2);

/// Startup error: a PV that did not connect, a socket that would not bind.
#[derive(Debug)]
pub struct Error(String);

impl Error {
    pub fn new(msg: impl Into<String>) -> Self {
        Self(msg.into())
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Error {}

impl From<bsrs::core::BsrsError> for Error {
    fn from(e: bsrs::core::BsrsError) -> Self {
        Self(e.to_string())
    }
}

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Parser, Debug)]
#[command(name = "bl5a-qs", version, about, long_about = None)]
struct Args {
    /// Control REP socket address.
    #[arg(long, default_value = "tcp://*:60715")]
    control: String,

    /// Document PUB socket address.
    #[arg(long, default_value = "tcp://*:60725")]
    documents: String,

    /// The folder each queue item's data folder goes in unless its
    /// `md.data_root` names another: an existing absolute directory
    /// without spaces, as mar345dtb takes a frame's path in one word.
    #[arg(long)]
    data_root: PathBuf,
}

fn main() {
    // A move's put and a SPEC command's put complete when the record
    // does, which for a monitor-count exposure is minutes; the CA
    // client's default put timeout would cut them.
    if std::env::var_os("EPICS_CA_PUT_TIMEOUT").is_none() {
        // SAFETY: no other thread exists yet.
        unsafe { std::env::set_var("EPICS_CA_PUT_TIMEOUT", "1e6") };
    }
    // The CA client is created with a one-off block_on, which panics
    // inside a running tokio runtime, so it goes first.
    bsrs::host::ca_devices::bootstrap_ca();
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");
    std::process::exit(rt.block_on(run(Args::parse())));
}

async fn run(args: Args) -> i32 {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .compact()
        .init();

    if !(args.data_root.is_absolute() && args.data_root.is_dir()) {
        eprintln!(
            "bl5a-qs: --data-root {}: not an existing absolute directory",
            args.data_root.display()
        );
        return 2;
    }
    if let Err(e) = place::frame_dir(&args.data_root) {
        eprintln!("bl5a-qs: --data-root {e}");
        return 2;
    }
    // The IOCs may come up after qs: wait for them, naming the PV awaited
    // when it changes, rather than exit as if the arguments were wrong.
    let mut awaited = String::new();
    let devices = loop {
        match Devices::connect().await {
            Ok(d) => break d,
            Err(e) => {
                let e = e.to_string();
                if e != awaited {
                    tracing::warn!(target: "bl5a-qs", "waiting for {e}");
                    awaited = e;
                }
                tokio::time::sleep(CONNECT_RETRY).await;
            }
        }
    };
    let mut reg = Registry::new();
    plans::register_all(&mut reg, &devices, &args.data_root);
    devices.register(&mut reg);
    tracing::info!(target: "bl5a-qs", devices = ?reg.device_names(), plans = ?reg.plan_names(), "registered");

    let mut builder = Server::builder()
        .control_address(&args.control)
        .document_address(&args.documents)
        .registry(reg);
    match specfile::Hook::start() {
        Ok(hook) => builder = builder.environment_hook(Arc::new(hook)),
        Err(e) => {
            eprintln!("bl5a-qs: SPEC file thread: {e}");
            return 2;
        }
    }
    let server = match builder.build() {
        Ok(s) => s,
        Err(e) => {
            eprintln!("bl5a-qs: server: {e}");
            return 2;
        }
    };
    let shutdown = server.shutdown_handle();
    tokio::spawn(async move {
        // SIGTERM too: its default disposition kills the daemon without
        // stopping the running plan.
        let mut term = signal(SignalKind::terminate()).ok();
        let terminated = async {
            match term.as_mut() {
                Some(t) => {
                    t.recv().await;
                }
                None => std::future::pending().await,
            }
        };
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = terminated => {}
        }
        eprintln!("\nbl5a-qs: shutting down");
        shutdown.shutdown();
    });

    println!(
        "bl5a-qs listening:\n  control:   {}\n  documents: {}",
        args.control, args.documents
    );
    match server.run_async().await {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("bl5a-qs: {e}");
            1
        }
    }
}
