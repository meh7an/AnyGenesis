//! The `anygenesis` child process a build runs in: its output lines arrive on a channel, and
//! Cancel kills it.

use std::{
    env,
    ffi::OsString,
    io::{self, BufRead, BufReader, Read},
    path::{Path, PathBuf},
    process::{self, Command, Stdio},
    sync::mpsc,
    thread,
};

/// `anygenesis` next to this program.
pub fn anygenesis() -> io::Result<PathBuf> {
    let exe = env::current_exe()?;
    let folder = exe.parent().unwrap_or(Path::new("."));
    Ok(folder.join(format!("anygenesis{}", env::consts::EXE_SUFFIX)))
}

/// The `patches` folder next to this program.
pub fn patches_folder() -> Option<PathBuf> {
    let exe = env::current_exe().ok()?;
    Some(exe.parent()?.join("patches"))
}

/// Keeps a console window from opening for a console program started from the GUI.
fn hide_console(command: &mut Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    #[cfg(not(windows))]
    let _ = command;
}

pub struct Child {
    process: process::Child,
    lines: mpsc::Receiver<String>,
    readers: Vec<thread::JoinHandle<()>>,
}

impl Child {
    /// Starts `program` with `arguments`, reading its standard output and error by line.
    pub fn spawn(program: &Path, arguments: &[OsString]) -> io::Result<Self> {
        let mut command = Command::new(program);
        command
            .args(arguments)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        hide_console(&mut command);
        let mut process = command.spawn()?;
        let (sender, lines) = mpsc::channel();
        let mut readers = Vec::new();
        let outputs: [Option<Box<dyn Read + Send>>; 2] = [
            process.stdout.take().map(|out| Box::new(out) as _),
            process.stderr.take().map(|err| Box::new(err) as _),
        ];
        for output in outputs.into_iter().flatten() {
            let sender = sender.clone();
            readers.push(thread::spawn(move || {
                let mut reader = BufReader::new(output);
                let mut line = Vec::new();
                while reader
                    .read_until(b'\n', &mut line)
                    .is_ok_and(|read| read > 0)
                {
                    let text = String::from_utf8_lossy(&line).trim_end().to_owned();
                    if sender.send(text).is_err() {
                        break;
                    }
                    line.clear();
                }
            }));
        }
        Ok(Self {
            process,
            lines,
            readers,
        })
    }

    /// The lines read since the last call.
    pub fn lines(&self) -> Vec<String> {
        self.lines.try_iter().collect()
    }

    /// The exit code once the process has ended (`Some(None)` when it had none), with every
    /// remaining line read first; `None` while it runs.
    pub fn exit(&mut self) -> Option<(Option<i32>, Vec<String>)> {
        let status = self.process.try_wait().ok()??;
        for reader in self.readers.drain(..) {
            let _ = reader.join();
        }
        Some((status.code(), self.lines()))
    }

    /// Ends the process; compilers it started finish their file and exit.
    pub fn kill(mut self) {
        let _ = self.process.kill();
        let _ = self.process.wait();
    }
}

/// Starts the game program `exe` from its own folder and lets it run on its own.
pub fn launch(exe: &Path) -> io::Result<()> {
    let mut command = Command::new(exe);
    if let Some(folder) = exe.parent() {
        command.current_dir(folder);
    }
    let mut game = command.spawn()?;
    thread::spawn(move || {
        let _ = game.wait();
    });
    Ok(())
}
