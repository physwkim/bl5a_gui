//! The mar345dtb socket the ad-mar345 driver talks to: one `\n`-terminated
//! command per line, answered by the completion line the driver waits for.
//!
//! The plate collects whatever the beam puts on the sample while the mar
//! shutter (PFCU1 blade 3) is out; `SCAN` reads that dose out into a file
//! and erases, `ERASE` only erases. Both take [`READOUT`] here — 70 to 108 s
//! on the real scanner — which still outlasts the 2 s the macros sleep
//! before polling the detector state.

use std::path::Path;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::time::Instant;

use super::file::{self, Meta};
use super::image::{Mode, Scene};
use super::rings::rings;
use crate::world::Shared;

const READOUT: Duration = Duration::from_secs(3);
const MODE_CHANGE: Duration = Duration::from_millis(500);
/// The driver's `waitForCompletion` strings, spacing included.
const DONE_MODE_CHANGE: &str = "MODE_CHANGE  Ended o.k.\n";
const DONE_SCAN_DATA: &str = "SCAN_DATA    Ended o.k.\n";

/// The scanner: its mode and how it renders, shared by every connection.
pub struct Scanner {
    shared: Arc<Shared>,
    scene: Arc<Mutex<Scene>>,
    mode: Mutex<Mode>,
}

impl Scanner {
    pub fn new(shared: Arc<Shared>, scene: Scene) -> Scanner {
        Scanner {
            shared,
            scene: Arc::new(Mutex::new(scene)),
            mode: Mutex::new(Mode::from_pixels(3450).expect("3450 is a mode")),
        }
    }

    fn mode(&self) -> Mode {
        *self.mode.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// One command line; the reply to send, if any.
    async fn command(&self, line: &str) -> Option<&'static str> {
        let words: Vec<&str> = line.split_whitespace().collect();
        match words[..] {
            ["COMMAND", "CHANGE", n] => {
                let Some(mode) = n.parse().ok().and_then(Mode::from_pixels) else {
                    eprintln!("bl5a-sim: mar: no scan mode {n}");
                    return None;
                };
                *self.mode.lock().unwrap_or_else(PoisonError::into_inner) = mode;
                tokio::time::sleep(MODE_CHANGE).await;
                Some(DONE_MODE_CHANGE)
            }
            ["COMMAND", "ERASE"] => {
                tokio::time::sleep(READOUT).await;
                self.shared.lock().take_dose();
                Some(DONE_SCAN_DATA)
            }
            ["COMMAND", "SCAN", path] => {
                let end = Instant::now() + READOUT;
                let wrote = self.scan(path).await;
                tokio::time::sleep_until(end).await;
                // The scan erases what arrived during it, too.
                self.shared.lock().take_dose();
                match wrote {
                    Ok(()) => Some(DONE_SCAN_DATA),
                    Err(e) => {
                        eprintln!("bl5a-sim: mar: {path}: {e}");
                        None
                    }
                }
            }
            // The beamline's shutter is PFCU1 blade 3, not the scanner's.
            ["COMMAND", "SHUTTER", "OPEN" | "CLOSE"] => None,
            _ => {
                eprintln!("bl5a-sim: mar: unknown command {line:?}");
                None
            }
        }
    }

    /// Reads the plate out into `path`.
    async fn scan(&self, path: &str) -> Result<(), String> {
        let mode = self.mode();
        let (dose, rings) = {
            let mut w = self.shared.lock();
            let rings = rings(w.sample, w.tc.reading, w.elapsed());
            (w.take_dose(), rings)
        };
        let d: Vec<String> = rings
            .iter()
            .map(|r| format!("{} {:?} {:.5}", r.phase, r.hkl, r.d))
            .collect();
        eprintln!(
            "bl5a-sim: mar: scan {path} mode {} photons {:.4e} open {:.2} s, d/Å: {}",
            mode.pixels,
            dose.photons,
            dose.open,
            d.join(", ")
        );
        let scene = self.scene.clone();
        let path = path.to_string();
        let date = chrono::Local::now()
            .format("%a %b %e %H:%M:%S %Y")
            .to_string();
        tokio::task::spawn_blocking(move || {
            let mut scene = scene.lock().unwrap_or_else(PoisonError::into_inner);
            let data = scene.render(mode, dose, &rings);
            let meta = Meta {
                mode,
                wavelength: scene.wavelength,
                distance: scene.distance(),
                center: scene.center(mode),
                time: dose.open,
                date,
            };
            drop(scene);
            std::fs::write(Path::new(&path), file::encode(&data, &meta)).map_err(|e| e.to_string())
        })
        .await
        .map_err(|e| e.to_string())?
    }
}

pub async fn serve(scanner: Arc<Scanner>, listener: TcpListener) {
    loop {
        match listener.accept().await {
            Ok((sock, peer)) => {
                eprintln!("bl5a-sim: mar client connected from {peer}");
                tokio::spawn(connection(scanner.clone(), sock));
            }
            Err(e) => eprintln!("bl5a-sim: mar accept: {e}"),
        }
    }
}

async fn connection(scanner: Arc<Scanner>, sock: TcpStream) {
    let (rd, mut wr) = sock.into_split();
    let mut lines = BufReader::new(rd).lines();
    loop {
        let line = match lines.next_line().await {
            Ok(Some(l)) => l,
            Ok(None) => break,
            Err(e) => {
                eprintln!("bl5a-sim: mar read: {e}");
                break;
            }
        };
        let line = line.trim_end_matches('\r');
        eprintln!("bl5a-sim: mar <- {line:?}");
        if let Some(reply) = scanner.command(line).await
            && let Err(e) = wr.write_all(reply.as_bytes()).await
        {
            eprintln!("bl5a-sim: mar write: {e}");
            break;
        }
    }
    eprintln!("bl5a-sim: mar client disconnected");
}
