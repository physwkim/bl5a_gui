//! The SPEC server face: one task per client reading packets, one writing
//! them, and the executor that runs the shared command queue one command
//! at a time, as the SPEC command thread does.

mod cam;
pub mod cmd;

use std::sync::Arc;

use spec_protocol::{Cmd, Framing, Header, Packet, format_number, read_packet, write_packet};
use tokio::net::tcp::OwnedWriteHalf;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;

use crate::world::{Job, MotorProp, Prop, Shared};
use cmd::Exit;

pub async fn serve(shared: Arc<Shared>, listener: TcpListener, process: String) {
    let mut next = 0u64;
    loop {
        match listener.accept().await {
            Ok((sock, peer)) => {
                next += 1;
                eprintln!("bl5a-sim: client {next} connected from {peer}");
                tokio::spawn(connection(shared.clone(), sock, next, process.clone()));
            }
            Err(e) => eprintln!("bl5a-sim: accept: {e}"),
        }
    }
}

/// Runs queued commands in order. `status/ready` reads busy from the
/// first command taken until the queue is empty again.
pub async fn executor(shared: Arc<Shared>, mut queue: mpsc::UnboundedReceiver<Job>) {
    let mut cam = cam::Cam::default();
    while let Some(job) = queue.recv().await {
        if job.abort_gen == shared.abort_gen() {
            shared.lock().busy = true;
            eprintln!("bl5a-sim: run {:?}", job.text);
            let out = cmd::run(&shared, &mut cam, &job.text, job.abort_gen).await;
            if let Err(e) = &out.result {
                eprintln!("bl5a-sim: {:?} ended: {e:?}", job.text);
            }
            if let Some((tx, request)) = job.reply {
                let _ = tx.send(reply(&request, &out.value, &out.result));
            }
        } else if let Some((tx, request)) = job.reply {
            // Queued before a ^C: removed unrun, as the server does.
            let _ = tx.send(reply(&request, "", &Err(Exit::Abort)));
        }
        if queue.is_empty() {
            shared.lock().busy = false;
        }
    }
}

fn reply(request: &Header, value: &str, result: &Result<(), Exit>) -> Packet {
    let p = Packet::reply_to(request);
    match result {
        Ok(()) => p.with_string(value),
        Err(e) => {
            let message = match e {
                Exit::Error(m) => m.as_str(),
                Exit::Abort => "^C",
            };
            let mut p = p.with_error(message);
            p.header.err = 1;
            p
        }
    }
}

struct Client {
    shared: Arc<Shared>,
    id: u64,
    process: String,
    tx: mpsc::UnboundedSender<Packet>,
    /// `prestart_all` ... `start_all`: the move being collected.
    prestart: Option<String>,
}

async fn connection(shared: Arc<Shared>, sock: TcpStream, id: u64, process: String) {
    let _ = sock.set_nodelay(true);
    let (mut rd, wr) = sock.into_split();
    let (tx, rx) = mpsc::unbounded_channel::<Packet>();
    let mut client = Client {
        shared: shared.clone(),
        id,
        process,
        tx,
        prestart: None,
    };
    let mut unstarted = Some((wr, rx));
    let mut writer = None;
    loop {
        let packet = match read_packet(&mut rd).await {
            Ok((packet, framing)) => {
                if let Some((wr, rx)) = unstarted.take() {
                    // Everything sent back uses the client's byte order and
                    // header version.
                    writer = Some(tokio::spawn(write_all(wr, framing, rx)));
                    shared.lock().connect(id, client.tx.clone());
                }
                packet
            }
            Err(e) if e.is_fatal() => {
                eprintln!("bl5a-sim: client {id}: {e}");
                break;
            }
            Err(e) => {
                eprintln!("bl5a-sim: client {id}: skipped: {e}");
                continue;
            }
        };
        if !client.handle(packet) {
            break;
        }
    }
    shared.lock().disconnect(id);
    if let Some(w) = writer {
        w.abort();
    }
    eprintln!("bl5a-sim: client {id} closed");
}

async fn write_all(
    mut wr: OwnedWriteHalf,
    framing: Framing,
    mut rx: mpsc::UnboundedReceiver<Packet>,
) {
    while let Some(p) = rx.recv().await {
        if let Err(e) = write_packet(&mut wr, &p, framing).await {
            eprintln!("bl5a-sim: write {}: {e}", p.header.name);
            break;
        }
    }
}

/// Names `SV_FUNC` calls as functions, `name(a, b)`; the rest are
/// commands, `name a b`.
const FUNCTIONS: [&str; 4] = ["motor_par", "sleep", "do_sleep", "mar345expose"];

/// `SV_FUNC` data: the name and its arguments, separated by NULs.
fn func_text(p: &Packet) -> String {
    let mut items = p
        .data
        .split(|&b| b == 0)
        .map(|f| String::from_utf8_lossy(f).into_owned());
    let name = items.next().unwrap_or_default();
    let args: Vec<String> = items.filter(|a| !a.is_empty()).collect();
    if FUNCTIONS.contains(&name.as_str()) {
        format!("{name}({})", args.join(", "))
    } else if args.is_empty() {
        name
    } else {
        format!("{name} {}", args.join(" "))
    }
}

impl Client {
    /// False when the client closed the connection.
    fn handle(&mut self, p: Packet) -> bool {
        let h = &p.header;
        eprintln!(
            "bl5a-sim: client {} {:?} sn={} {} {:?}",
            self.id,
            h.cmd,
            h.sn,
            h.name,
            p.text()
        );
        match h.cmd {
            Cmd::Hello => {
                let r =
                    Packet::new(Cmd::HelloReply, h.sn, h.name.clone()).with_string(&self.process);
                let _ = self.tx.send(r);
            }
            Cmd::Close => return false,
            Cmd::Abort => self.shared.abort(),
            Cmd::Cmd => self.shared.push(p.text().into_owned(), None),
            Cmd::CmdWithReturn => self
                .shared
                .push(p.text().into_owned(), Some((self.tx.clone(), h.clone()))),
            Cmd::Func => self.shared.push(func_text(&p), None),
            Cmd::FuncWithReturn => self
                .shared
                .push(func_text(&p), Some((self.tx.clone(), h.clone()))),
            Cmd::Register => self.shared.lock().register(self.id, &h.name, h.sn),
            Cmd::Unregister => self.shared.lock().unregister(self.id, &h.name),
            Cmd::ChanRead => self.chan_read(h),
            Cmd::ChanSend => self.chan_send(&p),
            Cmd::Return | Cmd::Event | Cmd::Reply | Cmd::HelloReply => {
                eprintln!(
                    "bl5a-sim: client {}: ignored {:?} from a client",
                    self.id, h.cmd
                );
            }
        }
        true
    }

    fn chan_read(&self, h: &Header) {
        let r = Packet::reply_to(h);
        let r = {
            let w = self.shared.lock();
            match w.resolve(&h.name) {
                Ok(prop) => match w.read(&prop) {
                    Some(v) => v.fill(r),
                    None => r.with_error(&format!("\"{}\" cannot be read", h.name)),
                },
                Err(e) => r.with_error(&e),
            }
        };
        let _ = self.tx.send(r);
    }

    fn chan_send(&mut self, p: &Packet) {
        let name = p.header.name.as_str();
        let resolved = self.shared.lock().resolve(name);
        let prop = match resolved {
            Ok(prop) => prop,
            Err(e) => return self.shared.lock().report_error(self.id, &e),
        };
        let value = p.number();
        let number = || value.ok_or(format!("\"{}\" is not a number for {name}", p.text()));
        let read_only = || format!("\"{name}\" is read only");
        let result = match prop {
            Prop::Motor(i, mp) => number().and_then(|v| {
                self.motor_send(i, mp, v)
                    .then_some(())
                    .ok_or_else(read_only)
            }),
            Prop::MotorAll(op) => {
                self.motor_all(&op);
                Ok(())
            }
            Prop::Count => number().map(|v| {
                if v == 0.0 {
                    self.shared.abort();
                } else {
                    self.shared
                        .push(format!("count_em {}\n", format_number(v)), None);
                }
            }),
            Prop::Var(var) => {
                self.shared.lock().set_var(&var, p.text());
                Ok(())
            }
            _ => Err(read_only()),
        };
        if let Err(e) = result {
            self.shared.lock().report_error(self.id, &e);
        }
    }

    /// False for a property that cannot be sent.
    fn motor_send(&mut self, i: usize, prop: MotorProp, v: f64) -> bool {
        let (mne, sign, dial) = {
            let w = self.shared.lock();
            let m = &w.motors[i];
            (m.mne.clone(), m.sign, m.dial())
        };
        let v_text = format_number(v);
        let text = match prop {
            MotorProp::StartOne => match &mut self.prestart {
                Some(buf) => {
                    buf.push_str(&format!("A[{mne}]={v_text};"));
                    return true;
                }
                None => format!("{{get_angles;A[{mne}]={v_text};move_em;}}\n"),
            },
            MotorProp::Position => format!("set {mne} {v_text}\n"),
            MotorProp::DialPosition => format!("set_dial {mne} {v_text}\n"),
            MotorProp::Offset => format!("set {mne} {}\n", format_number(sign * dial + v)),
            MotorProp::HighLimit | MotorProp::LowLimit => format!("set_lm {mne} {v_text}\n"),
            p => match p.par() {
                Some(par) => format!("motor_par({mne}, \"{par}\", {v_text})\n"),
                None => return false,
            },
        };
        self.shared.push(text, None);
        true
    }

    fn motor_all(&mut self, op: &str) {
        match op {
            "prestart_all" => self.prestart = Some("{getangles;".to_string()),
            "start_all" => {
                if let Some(mut buf) = self.prestart.take() {
                    buf.push_str("move_em;}\n");
                    self.shared.push(buf, None);
                }
            }
            "abort_all" => self.shared.abort(),
            _ => {
                let e = format!("Unknown property \"motor/../{op}\"");
                self.shared.lock().report_error(self.id, &e);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::world::tests::world;

    async fn start() -> TcpStream {
        let (shared, queue) = Shared::new(world());
        let shared = Arc::new(shared);
        let ticker = shared.clone();
        tokio::spawn(async move { ticker.tick().await });
        tokio::spawn(executor(shared.clone(), queue));
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(serve(shared, listener, "kappa".into()));
        TcpStream::connect(addr).await.unwrap()
    }

    async fn send(s: &mut TcpStream, p: Packet) {
        write_packet(s, &p, Framing::CLIENT).await.unwrap();
    }

    async fn chan_send(s: &mut TcpStream, name: &str, value: &str) {
        send(s, Packet::new(Cmd::ChanSend, 1, name).with_string(value)).await;
    }

    async fn recv(s: &mut TcpStream) -> Packet {
        read_packet(s).await.unwrap().0
    }

    /// Events read until `name` carries `value`, that one included.
    async fn until(s: &mut TcpStream, name: &str, value: &str) -> Vec<(String, String)> {
        let mut seen = Vec::new();
        loop {
            let p = recv(s).await;
            assert_eq!(p.header.cmd, Cmd::Event, "{p:?}");
            let e = (p.header.name.clone(), p.text().into_owned());
            let last = e.0 == name && e.1 == value;
            seen.push(e);
            if last {
                return seen;
            }
        }
    }

    fn last_of<'a>(seen: &'a [(String, String)], name: &str) -> Option<&'a str> {
        seen.iter()
            .rev()
            .find(|e| e.0 == name)
            .map(|e| e.1.as_str())
    }

    #[tokio::test(start_paused = true)]
    async fn hello_is_answered_with_the_process_name() {
        let mut s = start().await;
        send(&mut s, Packet::new(Cmd::Hello, 7, "")).await;
        let r = recv(&mut s).await;
        assert_eq!((r.header.cmd, r.header.sn), (Cmd::HelloReply, 7));
        assert_eq!(r.text(), "kappa");
    }

    #[tokio::test(start_paused = true)]
    async fn start_one_moves_and_reports_the_position_before_done() {
        let mut s = start().await;
        for name in ["motor/tth/position", "motor/tth/move_done"] {
            send(&mut s, Packet::new(Cmd::Register, 1, name)).await;
        }
        until(&mut s, "motor/tth/move_done", "0").await;
        chan_send(&mut s, "motor/tth/start_one", "2").await;
        until(&mut s, "motor/tth/move_done", "1").await;
        let seen = until(&mut s, "motor/tth/move_done", "0").await;
        assert_eq!(last_of(&seen, "motor/tth/position"), Some("2"));
    }

    #[tokio::test(start_paused = true)]
    async fn prestart_collects_motors_into_one_move() {
        let mut s = start().await;
        for name in ["motor/th/move_done", "motor/sy/move_done"] {
            send(&mut s, Packet::new(Cmd::Register, 1, name)).await;
            until(&mut s, name, "0").await;
        }
        chan_send(&mut s, "motor/../prestart_all", "0").await;
        chan_send(&mut s, "motor/th/start_one", "0.5").await;
        chan_send(&mut s, "motor/sy/start_one", "0.1").await;
        chan_send(&mut s, "motor/../start_all", "0").await;
        let mut seen = until(&mut s, "motor/th/move_done", "1").await;
        seen.extend(until(&mut s, "motor/sy/move_done", "1").await);
        // Both start in the one command: neither finishes in between.
        assert!(
            !seen
                .iter()
                .any(|e| e.1 == "0" && e.0.ends_with("move_done"))
        );
        let mut done = 0;
        while done < 2 {
            let p = recv(&mut s).await;
            if p.text() == "0" {
                done += 1;
            }
        }
        for (name, want) in [("motor/th/position", "0.5"), ("motor/sy/position", "0.1")] {
            send(&mut s, Packet::new(Cmd::ChanRead, 9, name)).await;
            let r = recv(&mut s).await;
            assert_eq!((r.header.cmd, r.header.name.as_str()), (Cmd::Reply, name));
            assert_eq!(r.text(), want);
        }
    }

    #[tokio::test(start_paused = true)]
    async fn count_runs_for_the_time_preset() {
        let mut s = start().await;
        send(&mut s, Packet::new(Cmd::Register, 1, "scaler/.all./count")).await;
        until(&mut s, "scaler/.all./count", "0").await;
        chan_send(&mut s, "scaler/.all./count", "1.5").await;
        until(&mut s, "scaler/.all./count", "1").await;
        until(&mut s, "scaler/.all./count", "0").await;
        send(&mut s, Packet::new(Cmd::ChanRead, 3, "scaler/sec/value")).await;
        let r = recv(&mut s).await;
        assert_eq!((r.header.cmd, r.header.sn), (Cmd::Reply, 3));
        assert_eq!(r.text(), "1.5");
    }

    #[tokio::test(start_paused = true)]
    async fn abort_stops_a_move_short() {
        let mut s = start().await;
        send(&mut s, Packet::new(Cmd::Register, 1, "motor/tth/move_done")).await;
        until(&mut s, "motor/tth/move_done", "0").await;
        chan_send(&mut s, "motor/tth/start_one", "60").await;
        until(&mut s, "motor/tth/move_done", "1").await;
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        send(&mut s, Packet::new(Cmd::Abort, 1, "")).await;
        until(&mut s, "motor/tth/move_done", "0").await;
        send(&mut s, Packet::new(Cmd::ChanRead, 2, "motor/tth/position")).await;
        let tth: f64 = recv(&mut s).await.text().parse().unwrap();
        assert!(tth > 1.0 && tth < 60.0, "{tth}");
    }

    #[tokio::test(start_paused = true)]
    async fn command_replies_carry_the_value_or_the_error() {
        let mut s = start().await;
        let call = |sn, text: &str| Packet::new(Cmd::CmdWithReturn, sn, "").with_string(text);
        send(&mut s, call(1, "motor_par(th, \"slew_rate\", 3000)")).await;
        send(&mut s, call(2, "mar_ip7 1234 835 100 0 50 1.2 -6.8")).await;
        send(&mut s, call(3, "mymar 1")).await;
        let func = Packet::new(Cmd::FuncWithReturn, 4, "").with_string("motor_par\0th\0slew_rate");
        send(&mut s, func).await;
        let mut replies = Vec::new();
        for _ in 0..4 {
            let r = recv(&mut s).await;
            assert_eq!(r.header.cmd, Cmd::Reply);
            replies.push((r.header.sn, r.header.err, r.text().into_owned()));
        }
        assert_eq!(
            replies,
            [
                (1, 0, "3000".into()),
                (
                    2,
                    1,
                    " Two Theta Angle is too low for UPWARD Geometry.".into()
                ),
                (3, 1, "mymar: not modeled by bl5a-sim".into()),
                (4, 0, "3000".into()),
            ]
        );
    }

    #[tokio::test(start_paused = true)]
    async fn velocity_writes_leave_the_config_velocity() {
        let mut s = start().await;
        let func = |sn, args: &str| Packet::new(Cmd::FuncWithReturn, sn, "").with_string(args);
        send(&mut s, func(1, "motor_par\0th\0velocity\x003000")).await;
        send(&mut s, func(2, "motor_par\0th\0slew_rate")).await;
        send(&mut s, func(3, "motor_par\0th\0config_velocity")).await;
        let mut replies = Vec::new();
        for _ in 0..3 {
            let r = recv(&mut s).await;
            replies.push((r.header.sn, r.header.err, r.text().into_owned()));
        }
        assert_eq!(
            replies,
            [
                (1, 0, "3000".into()),
                (2, 0, "3000".into()),
                (3, 0, "4000".into())
            ]
        );
    }

    #[tokio::test(start_paused = true)]
    async fn unknown_names_are_reported_on_the_error_watch() {
        let mut s = start().await;
        send(&mut s, Packet::new(Cmd::Register, 1, "error")).await;
        until(&mut s, "error", "No error").await;
        chan_send(&mut s, "motor/yy/start_one", "1").await;
        until(&mut s, "error", "Unknown property \"motor/yy/start_one\"").await;
        chan_send(&mut s, "motor/tth/move_done", "1").await;
        until(&mut s, "error", "\"motor/tth/move_done\" is read only").await;
    }
}
