//! Single instance: a second start (e.g. a double-click in Explorer) hands its files to the running Kladde and
//! exits. The running instance listens on a loopback port that it records, with a random token, in the data
//! directory; portable copies have their own data directory and so stay separate.

use std::collections::hash_map::RandomState;
use std::hash::{BuildHasher, Hasher};
use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver};
use std::time::Duration;

const INSTANCE_FILE: &str = "instance";
/// A running instance answers within milliseconds; a hung one must not keep the new start waiting long.
const TIMEOUT: Duration = Duration::from_secs(2);
/// Far above any real file list; stops a stray client from filling memory.
const REQUEST_MAX: u64 = 1 << 20;
const ACCEPTED: &str = "ok";

/// Listening side of the running instance, created before the window so a second start can find it.
pub struct Server {
    listener: TcpListener,
    token: String,
}

/// Outcome of a start: the files went to a running instance, or this process opens the window.
pub enum Start {
    Forwarded,
    Primary(Option<Server>),
}

/// Forwards `args` to a running instance, or becomes the instance. Errors only disable the single-instance
/// behaviour, so they are returned with the server missing rather than stopping the start.
pub fn start(data_dir: &Path, args: &[PathBuf]) -> Start {
    let file = data_dir.join(INSTANCE_FILE);
    let forwarded = std::fs::read_to_string(&file).ok().is_some_and(|recorded| forward(&recorded, args));
    match forwarded {
        true => Start::Forwarded,
        false => Start::Primary(listen(&file).ok()),
    }
}

impl Server {
    /// Serves forwarded file lists on a background thread; each one wakes `ctx`.
    pub fn serve(self, ctx: egui::Context) -> Receiver<Vec<PathBuf>> {
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            self.listener.incoming().filter_map(Result::ok).for_each(|stream| {
                // A broken or foreign connection only affects that client.
                if let Ok(paths) = receive(stream, &self.token) {
                    if tx.send(paths).is_ok() {
                        ctx.request_repaint();
                    }
                }
            });
        });
        rx
    }
}

fn listen(file: &Path) -> io::Result<Server> {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
    let token = new_token();
    std::fs::write(file, format!("{} {token}", listener.local_addr()?.port()))?;
    Ok(Server { listener, token })
}

/// Sends the token and absolute paths; true once the instance confirmed. Paths that cannot travel as a line
/// (not UTF-8, or containing a line break) make this start open its own window instead.
fn forward(recorded: &str, args: &[PathBuf]) -> bool {
    let lines: Option<Vec<String>> = args
        .iter()
        .map(|p| std::path::absolute(p).ok()?.to_str().filter(|s| !s.contains(['\n', '\r'])).map(str::to_owned))
        .collect();
    let target = recorded.split_once(' ').and_then(|(port, token)| Some((port.parse::<u16>().ok()?, token)));
    match (lines, target) {
        (Some(lines), Some((port, token))) => send(port, token, &lines).is_ok_and(|reply| reply == ACCEPTED),
        _ => false,
    }
}

fn send(port: u16, token: &str, lines: &[String]) -> io::Result<String> {
    allow_focus_steal();
    let mut stream = TcpStream::connect_timeout(&SocketAddr::from((Ipv4Addr::LOCALHOST, port)), TIMEOUT)?;
    stream.set_read_timeout(Some(TIMEOUT))?;
    stream.set_write_timeout(Some(TIMEOUT))?;
    let mut request = format!("{token}\n");
    lines.iter().for_each(|l| {
        request.push_str(l);
        request.push('\n');
    });
    stream.write_all(request.as_bytes())?;
    stream.shutdown(std::net::Shutdown::Write)?;
    let mut reply = String::new();
    stream.take(16).read_to_string(&mut reply)?;
    Ok(reply)
}

fn receive(stream: TcpStream, token: &str) -> io::Result<Vec<PathBuf>> {
    stream.set_read_timeout(Some(TIMEOUT))?;
    stream.set_write_timeout(Some(TIMEOUT))?;
    let mut lines = BufReader::new((&stream).take(REQUEST_MAX)).lines();
    let authorised = lines.next().transpose()?.is_some_and(|first| first == token);
    if !authorised {
        return Err(io::Error::new(io::ErrorKind::PermissionDenied, "wrong instance token"));
    }
    let paths = lines.map(|l| l.map(PathBuf::from)).collect::<io::Result<Vec<_>>>()?;
    (&stream).write_all(ACCEPTED.as_bytes())?;
    Ok(paths)
}

/// Unguessable enough to keep other local users' processes from opening files here; std seeds
/// `RandomState` from the OS.
fn new_token() -> String {
    let part = || {
        let mut h = RandomState::new().build_hasher();
        h.write_u32(std::process::id());
        h.finish()
    };
    format!("{:016x}{:016x}", part(), part())
}

/// Windows only lets the foreground process raise a window; the new start is foreground (the user just
/// launched it) and passes that right to the running instance.
#[cfg(windows)]
fn allow_focus_steal() {
    const ASFW_ANY: u32 = u32::MAX;
    #[link(name = "user32")]
    extern "system" {
        fn AllowSetForegroundWindow(process_id: u32) -> i32;
    }
    // SAFETY: plain Win32 call without pointers; failure only means the window may not come to the front.
    unsafe {
        AllowSetForegroundWindow(ASFW_ANY);
    }
}

#[cfg(not(windows))]
fn allow_focus_steal() {}
