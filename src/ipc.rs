//! Local control socket: `ddpm brightness ±N` hands the step to the running app,
//! which owns the i2c handles. Commands go straight to the DDC worker, so they
//! work while the window is minimized and the UI thread gets no repaints.
//!
//! The protocol is one command per line, currently only `brightness <delta>`.

use crate::Cmd;
use std::io::{self, BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;
use std::thread;
use std::time::Duration;

/// A client that connects and then says nothing must not block the others for long.
const CLIENT_TIMEOUT: Duration = Duration::from_secs(1);

fn socket_path() -> PathBuf {
    std::env::var_os("XDG_RUNTIME_DIR")
        .map_or_else(std::env::temp_dir, PathBuf::from)
        .join("ddpm.sock")
}

/// Send a brightness step to the running app.
pub fn send_brightness(delta: i32) -> io::Result<()> {
    let mut stream = UnixStream::connect(socket_path())?;
    writeln!(stream, "brightness {delta}")
}

/// Accept commands on a background thread for as long as the worker is alive.
pub fn spawn(cmds: Sender<Cmd>) {
    let path = socket_path();
    let listener = match bind(&path) {
        Ok(Some(listener)) => listener,
        Ok(None) => {
            log::warn!("another ddpm owns {}; not listening", path.display());
            return;
        }
        Err(e) => {
            log::warn!("cannot listen on {}: {e}", path.display());
            return;
        }
    };
    thread::Builder::new()
        .name("ipc".into())
        .spawn(move || serve(listener, cmds))
        .expect("failed to spawn IPC thread");
}

/// Bind the socket, replacing a stale file. `None` if a live instance holds it.
fn bind(path: &Path) -> io::Result<Option<UnixListener>> {
    if UnixStream::connect(path).is_ok() {
        return Ok(None);
    }
    match std::fs::remove_file(path) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e),
        _ => {}
    }
    let listener = UnixListener::bind(path)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    Ok(Some(listener))
}

fn serve(listener: UnixListener, cmds: Sender<Cmd>) {
    for stream in listener.incoming().flatten() {
        let _ = stream.set_read_timeout(Some(CLIENT_TIMEOUT));
        for line in BufReader::new(stream).lines().map_while(Result::ok) {
            let Some(cmd) = parse_command(&line) else {
                log::warn!("ignoring unknown command {line:?}");
                continue;
            };
            if cmds.send(cmd).is_err() {
                return;
            }
        }
    }
}

fn parse_command(line: &str) -> Option<Cmd> {
    let delta = line.strip_prefix("brightness ")?.parse().ok()?;
    Some(Cmd::Nudge { delta })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    fn delta(cmd: Option<Cmd>) -> Option<i32> {
        match cmd {
            Some(Cmd::Nudge { delta }) => Some(delta),
            _ => None,
        }
    }

    #[test]
    fn parses_brightness_steps() {
        assert_eq!(delta(parse_command("brightness 5")), Some(5));
        assert_eq!(delta(parse_command("brightness -5")), Some(-5));
        assert_eq!(delta(parse_command("brightness")), None);
        assert_eq!(delta(parse_command("brightness up")), None);
        assert_eq!(delta(parse_command("contrast 5")), None);
    }

    #[test]
    fn socket_delivers_commands_and_skips_garbage() {
        let path = std::env::temp_dir().join(format!("ddpm-test-{}.sock", std::process::id()));
        // A file left behind by a crash must not stop the next start.
        std::fs::write(&path, "stale").unwrap();
        let listener = bind(&path).unwrap().expect("stale file is replaced");
        assert!(bind(&path).unwrap().is_none(), "live socket is left alone");

        let (tx, rx) = mpsc::channel();
        thread::spawn(move || serve(listener, tx));
        let mut client = UnixStream::connect(&path).unwrap();
        writeln!(client, "nonsense\nbrightness -5\nbrightness 10").unwrap();
        drop(client);

        let timeout = Duration::from_secs(5);
        assert_eq!(delta(rx.recv_timeout(timeout).ok()), Some(-5));
        assert_eq!(delta(rx.recv_timeout(timeout).ok()), Some(10));
        std::fs::remove_file(&path).unwrap();
    }
}
