mod beam;
mod config;
mod lakeshore;
mod mar;
mod motor;
mod spec;
mod world;

use std::collections::HashMap;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use tokio::net::TcpListener;

use beam::Sample;
use config::Config;
use world::{Shared, Start, World};

const USAGE: &str = "\
usage: bl5a-sim [--config FILE] [--bind ADDR] [--port N] [--name PROC]
                [--mar-port N] [--poni FILE] [--seed N] [--sample coin|film]
                [--pos MNE=USER]... [--limit MNE=LO,HI]... [--hw-limit MNE=LO,HI]...
Limits are dial units, as SPEC keeps them.";

struct Opts {
    config: String,
    bind: String,
    port: u16,
    name: String,
    mar_port: u16,
    poni: String,
    start: Start,
}

fn pair(v: &str) -> Result<(String, &str), String> {
    let (k, v) = v.split_once('=').ok_or(format!("'{v}' is not MNE=VALUE"))?;
    Ok((k.to_string(), v))
}

fn number(v: &str) -> Result<f64, String> {
    v.trim()
        .parse()
        .map_err(|_| format!("'{v}' is not a number"))
}

fn range(v: &str) -> Result<(f64, f64), String> {
    let (lo, hi) = v.split_once(',').ok_or(format!("'{v}' is not LO,HI"))?;
    Ok((number(lo)?, number(hi)?))
}

impl Opts {
    fn parse(mut args: impl Iterator<Item = String>) -> Result<Opts, String> {
        let seed = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(1, |d| d.as_nanos() as u64);
        let mut opts = Opts {
            config: "ref/spec/kappa.config".into(),
            bind: "127.0.0.1".into(),
            port: spec_protocol::PORT_FIRST,
            name: "kappa".into(),
            mar_port: 5001,
            poni: "sim/bl5a.poni".into(),
            start: Start::beamline(Sample::Film, seed),
        };
        let mut limits = HashMap::new();
        let mut switches = HashMap::new();
        while let Some(flag) = args.next() {
            if flag == "-h" || flag == "--help" {
                return Err(String::new());
            }
            let v = args.next().ok_or(format!("{flag} needs a value"))?;
            match flag.as_str() {
                "--config" => opts.config = v,
                "--bind" => opts.bind = v,
                "--port" => opts.port = v.parse().map_err(|_| format!("bad port '{v}'"))?,
                "--name" => opts.name = v,
                "--mar-port" => opts.mar_port = v.parse().map_err(|_| format!("bad port '{v}'"))?,
                "--poni" => opts.poni = v,
                "--seed" => opts.start.seed = v.parse().map_err(|_| format!("bad seed '{v}'"))?,
                "--sample" => {
                    opts.start.sample = match v.as_str() {
                        "coin" => Sample::Coin,
                        "film" => Sample::Film,
                        _ => return Err(format!("unknown sample '{v}'")),
                    }
                }
                "--pos" => {
                    let (m, x) = pair(&v)?;
                    opts.start.positions.insert(m, number(x)?);
                }
                "--limit" => {
                    let (m, r) = pair(&v)?;
                    limits.insert(m, range(r)?);
                }
                "--hw-limit" => {
                    let (m, r) = pair(&v)?;
                    switches.insert(m, range(r)?);
                }
                _ => return Err(format!("unknown option {flag}")),
            }
        }
        opts.start.limits = limits;
        opts.start.switches = switches;
        Ok(opts)
    }
}

async fn run(opts: Opts) -> Result<(), String> {
    let text =
        std::fs::read_to_string(&opts.config).map_err(|e| format!("{}: {e}", opts.config))?;
    let cfg = Config::parse(&text).map_err(|e| format!("{}: {e}", opts.config))?;
    for mne in cfg.duplicate_motors() {
        eprintln!("bl5a-sim: motor {mne} is configured twice; the first is used");
    }
    for mne in opts.start.positions.keys().chain(opts.start.limits.keys()) {
        if cfg.motor(mne).is_none() {
            return Err(format!("no motor {mne} in {}", opts.config));
        }
    }
    let world = World::new(cfg, &opts.start)?;
    // Its own stream, so frames do not shift the counters' noise.
    let scene = mar::Scene::load(&opts.poni, opts.start.seed ^ 0x6d61_7233_3435)?;
    eprintln!(
        "bl5a-sim: seed {} sample {:?} truth {:?}",
        opts.start.seed, world.sample, world.truth
    );
    let listener = TcpListener::bind((opts.bind.as_str(), opts.port))
        .await
        .map_err(|e| format!("bind {}:{}: {e}", opts.bind, opts.port))?;
    let mar = TcpListener::bind((opts.bind.as_str(), opts.mar_port))
        .await
        .map_err(|e| format!("bind {}:{}: {e}", opts.bind, opts.mar_port))?;
    eprintln!(
        "bl5a-sim: SPEC server '{}' on {}:{}, mar345dtb on port {} ({})",
        opts.name, opts.bind, opts.port, opts.mar_port, opts.poni
    );
    let (shared, queue) = Shared::new(world);
    let shared = Arc::new(shared);
    let scanner = Arc::new(mar::Scanner::new(shared.clone(), scene));
    tokio::spawn(mar::serve(scanner, mar));
    let ticker = shared.clone();
    tokio::spawn(async move { ticker.tick().await });
    tokio::spawn(spec::executor(shared.clone(), queue));
    spec::serve(shared, listener, opts.name).await;
    Ok(())
}

#[tokio::main]
async fn main() -> ExitCode {
    let opts = match Opts::parse(std::env::args().skip(1)) {
        Ok(o) => o,
        Err(e) => {
            if !e.is_empty() {
                eprintln!("bl5a-sim: {e}");
            }
            eprintln!("{USAGE}");
            return ExitCode::from(2);
        }
    };
    match run(opts).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("bl5a-sim: {e}");
            ExitCode::FAILURE
        }
    }
}
