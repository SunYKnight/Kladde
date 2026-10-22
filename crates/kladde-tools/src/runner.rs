use std::ffi::OsString;
use std::fs::OpenOptions;
use std::io::{BufRead, BufReader, Read, Write};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::process_tree::ProcessTree;
use crate::{find_program, hide_window, ToolError};

/// Grandchildren can keep the output pipes open after the main process exited; wait at most this long for the
/// readers before reporting `Exited`, so late output usually precedes it but never blocks it.
const DRAIN_TIMEOUT: Duration = Duration::from_millis(500);

#[derive(Clone, Debug, PartialEq)]
pub struct FenceBlock {
    pub lang: String,
    pub header_line: usize,
    /// Byte range of the body within the given text; an unclosed fence runs to the end of the text.
    pub content: Range<usize>,
    pub closed: bool,
}

struct OpenFence {
    marker: u8,
    len: usize,
    lang: String,
    header_line: usize,
    body_start: usize,
}

/// Finds Markdown code fences (``` and ~~~, at least three, up to three spaces indent). The language is the
/// lowercased first word of the info string; the Quarto form "```{python echo=false}" yields "python".
pub fn find_fences(text: &str) -> Vec<FenceBlock> {
    let mut blocks = Vec::new();
    let mut open: Option<OpenFence> = None;
    let mut offset = 0;
    for (index, raw) in text.split_inclusive('\n').enumerate() {
        let line = raw.trim_end_matches(['\n', '\r']);
        let line_end = offset + raw.len();
        match open.take() {
            None => {
                open = parse_opener(line).map(|(marker, len, lang)| OpenFence {
                    marker,
                    len,
                    lang,
                    header_line: index,
                    body_start: line_end,
                });
            }
            Some(fence) => {
                if is_closer(line, &fence) {
                    blocks.push(FenceBlock {
                        lang: fence.lang,
                        header_line: fence.header_line,
                        content: fence.body_start..offset,
                        closed: true,
                    });
                } else {
                    open = Some(fence);
                }
            }
        }
        offset = line_end;
    }
    if let Some(fence) = open {
        blocks.push(FenceBlock {
            lang: fence.lang,
            header_line: fence.header_line,
            content: fence.body_start..text.len(),
            closed: false,
        });
    }
    blocks
}

/// Returns (marker byte, marker length, indent-stripped rest) when `line` starts with a fence marker.
fn fence_marker(line: &str) -> Option<(u8, usize, &str)> {
    let indent = line.len() - line.trim_start_matches(' ').len();
    let rest = &line[indent..];
    let marker = rest.bytes().next().filter(|b| *b == b'`' || *b == b'~')?;
    let len = rest.bytes().take_while(|b| *b == marker).count();
    (indent <= 3 && len >= 3).then_some((marker, len, &rest[len..]))
}

fn parse_opener(line: &str) -> Option<(u8, usize, String)> {
    let (marker, len, info) = fence_marker(line)?;
    // CommonMark: backticks in the info string mean inline code.
    let valid = marker == b'~' || !info.contains('`');
    valid.then(|| (marker, len, fence_lang(info)))
}

fn fence_lang(info: &str) -> String {
    let info = info.trim();
    let info = info.strip_prefix('{').unwrap_or(info);
    let info = info.strip_prefix('.').unwrap_or(info);
    let word: String = info.chars().take_while(|c| !c.is_whitespace() && !matches!(c, ',' | '}' | '{')).collect();
    word.to_lowercase()
}

fn is_closer(line: &str, fence: &OpenFence) -> bool {
    fence_marker(line)
        .is_some_and(|(marker, len, rest)| marker == fence.marker && len >= fence.len && rest.trim().is_empty())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Interpreter {
    PowerShell,
    Cmd,
    Bash,
    Python,
    Node,
    R,
}

impl Interpreter {
    pub fn from_lang(lang: &str) -> Option<Interpreter> {
        match lang.trim().to_lowercase().as_str() {
            "powershell" | "pwsh" | "ps" | "ps1" => Some(Interpreter::PowerShell),
            "cmd" | "bat" | "batch" => Some(Interpreter::Cmd),
            "bash" | "sh" | "shell" | "zsh" => Some(Interpreter::Bash),
            "python" | "py" | "python3" => Some(Interpreter::Python),
            "node" | "js" | "javascript" => Some(Interpreter::Node),
            "r" => Some(Interpreter::R),
            _ => None,
        }
    }

    /// Accepts the extension with or without the leading dot, case-insensitively.
    pub fn from_extension(ext: &str) -> Option<Interpreter> {
        let ext = ext.trim_start_matches('.').to_lowercase();
        match ext.as_str() {
            "ps1" => Some(Interpreter::PowerShell),
            "bat" | "cmd" => Some(Interpreter::Cmd),
            "sh" | "bash" => Some(Interpreter::Bash),
            "py" | "pyw" => Some(Interpreter::Python),
            "js" | "mjs" | "cjs" => Some(Interpreter::Node),
            "r" => Some(Interpreter::R),
            _ => None,
        }
    }

    pub fn all() -> &'static [Interpreter] {
        &[
            Interpreter::PowerShell,
            Interpreter::Cmd,
            Interpreter::Bash,
            Interpreter::Python,
            Interpreter::Node,
            Interpreter::R,
        ]
    }

    pub fn label(&self) -> &'static str {
        match self {
            Interpreter::PowerShell => "PowerShell",
            Interpreter::Cmd => "cmd",
            Interpreter::Bash => "Bash",
            Interpreter::Python => "Python",
            Interpreter::Node => "Node.js",
            Interpreter::R => "R",
        }
    }

    /// Location of the interpreter executable, if installed.
    pub fn program(&self) -> Option<PathBuf> {
        match self {
            Interpreter::PowerShell => find_program("pwsh").or_else(windows_powershell),
            Interpreter::Cmd => cmd_program(),
            Interpreter::Bash => bash_program(),
            Interpreter::Python => python_program(),
            Interpreter::Node => find_program("node"),
            Interpreter::R => find_program("Rscript"),
        }
    }

    fn script_extension(&self) -> &'static str {
        match self {
            Interpreter::PowerShell => "ps1",
            Interpreter::Cmd => "cmd",
            Interpreter::Bash => "sh",
            Interpreter::Python => "py",
            Interpreter::Node => "js",
            Interpreter::R => "R",
        }
    }

    fn require_program(&self) -> Result<PathBuf, ToolError> {
        self.program().ok_or_else(|| ToolError::NotFound(format!("{} interpreter", self.label())))
    }

    /// Arguments that make the interpreter execute `script` (cmd needs a raw command line, see `command`).
    fn script_args(&self, script: &Path) -> Vec<OsString> {
        match self {
            Interpreter::PowerShell => {
                let mut args: Vec<OsString> =
                    ["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-File"].map(OsString::from).into();
                args.push(script.as_os_str().to_os_string());
                args
            }
            Interpreter::Bash => vec![shell_path(script)],
            Interpreter::Cmd | Interpreter::Python | Interpreter::Node | Interpreter::R => {
                vec![script.as_os_str().to_os_string()]
            }
        }
    }

    /// A ready-to-spawn command executing `script` with piped output and no console window.
    fn command(&self, script: &Path, cwd: &Path) -> Result<Command, ToolError> {
        let program = self.require_program()?;
        let mut cmd = Command::new(program);
        if *self == Interpreter::Cmd {
            cmd_script_args(&mut cmd, script);
        } else {
            cmd.args(self.script_args(script));
        }
        cmd.current_dir(cwd);
        // Python buffers and uses the ANSI code page when writing to a pipe.
        cmd.env("PYTHONUNBUFFERED", "1").env("PYTHONIOENCODING", "utf-8");
        Ok(cmd)
    }

    /// Script text as written to the temp file: line endings the interpreter copes with, UTF-8 output.
    fn script_text(&self, code: &str) -> Vec<u8> {
        let mut text = code.replace("\r\n", "\n");
        if !text.ends_with('\n') {
            text.push('\n');
        }
        match self {
            // PS 5.1 reads BOM-less scripts as ANSI, writes pipes as OEM.
            Interpreter::PowerShell => {
                let mut bytes = vec![0xEF, 0xBB, 0xBF];
                let prefix = if cfg!(windows) { "[Console]::OutputEncoding = [Text.Encoding]::UTF8\n" } else { "" };
                bytes.extend_from_slice(prefix.as_bytes());
                bytes.extend_from_slice(text.as_bytes());
                bytes
            }
            // LF-only batch files break labels; chcp makes cmd emit UTF-8.
            Interpreter::Cmd => format!("@chcp 65001 >nul\r\n{}", text.replace('\n', "\r\n")).into_bytes(),
            Interpreter::Bash | Interpreter::Python | Interpreter::Node | Interpreter::R => text.into_bytes(),
        }
    }
}

#[cfg(windows)]
fn windows_powershell() -> Option<PathBuf> {
    find_program("powershell")
}

#[cfg(not(windows))]
fn windows_powershell() -> Option<PathBuf> {
    None
}

#[cfg(windows)]
fn cmd_program() -> Option<PathBuf> {
    std::env::var_os("COMSPEC").map(PathBuf::from).filter(|p| p.is_file()).or_else(|| find_program("cmd"))
}

#[cfg(not(windows))]
fn cmd_program() -> Option<PathBuf> {
    None
}

/// Git Bash next to the git installation; `System32\bash.exe` is WSL and must never be used.
#[cfg(windows)]
fn bash_program() -> Option<PathBuf> {
    let from_git = find_program("git").and_then(|git| {
        git.ancestors().skip(1).take(3).map(|dir| dir.join("bin").join("bash.exe")).find(|p| p.is_file())
    });
    from_git.or_else(|| {
        std::env::var_os("ProgramFiles")
            .map(|dir| PathBuf::from(dir).join("Git").join("bin").join("bash.exe"))
            .filter(|p| p.is_file())
    })
}

#[cfg(not(windows))]
fn bash_program() -> Option<PathBuf> {
    find_program("bash").or_else(|| find_program("sh"))
}

#[cfg(windows)]
fn python_program() -> Option<PathBuf> {
    find_program("python").or_else(|| find_program("py"))
}

#[cfg(not(windows))]
fn python_program() -> Option<PathBuf> {
    find_program("python3").or_else(|| find_program("python"))
}

/// MSYS bash understands drive paths with forward slashes reliably; backslashes are escapes for it.
fn shell_path(path: &Path) -> OsString {
    if cfg!(windows) {
        OsString::from(path.to_string_lossy().replace('\\', "/"))
    } else {
        path.as_os_str().to_os_string()
    }
}

#[cfg(windows)]
fn cmd_script_args(cmd: &mut Command, script: &Path) {
    use std::os::windows::process::CommandExt;
    // With /S cmd strips exactly the outer quotes.
    cmd.arg("/D").arg("/S").arg("/C").raw_arg(format!("\"\"{}\"\"", script.display()));
}

#[cfg(not(windows))]
fn cmd_script_args(cmd: &mut Command, script: &Path) {
    cmd.arg(script);
}

#[derive(Clone, Debug, PartialEq)]
pub enum RunEvent {
    Stdout(String),
    Stderr(String),
    Exited(Option<i32>),
}

/// A running (or finished) external process whose output arrives as `RunEvent`s.
pub struct RunHandle {
    title: String,
    tree: ProcessTree,
    events: Receiver<RunEvent>,
    running: Arc<AtomicBool>,
    /// Temp script removed by the waiter thread after exit.
    #[cfg_attr(not(test), allow(dead_code))]
    script: Option<PathBuf>,
}

impl RunHandle {
    pub fn title(&self) -> &str {
        &self.title
    }

    /// Events received since the last call; `Exited` is the last event of a run.
    pub fn poll(&self) -> Vec<RunEvent> {
        self.events.try_iter().collect()
    }

    pub fn is_running(&self) -> bool {
        self.running.load(Ordering::SeqCst)
    }

    /// Kills the process and everything it started; a no-op once the process has exited.
    pub fn kill(&self) -> Result<(), ToolError> {
        let result = if self.is_running() { self.tree.kill() } else { Ok(()) };
        // The process may exit between the check and the kill; that is success.
        match result {
            Err(_) if !self.is_running() => Ok(()),
            other => other,
        }
    }
}

/// Runs `code` with `interp` from a temp script in `cwd`. Output is streamed line by line as lossy UTF-8.
pub fn run_script(interp: Interpreter, code: &str, cwd: &Path) -> Result<RunHandle, ToolError> {
    let script = write_temp_script(interp, code)?;
    let started = interp.command(&script, cwd).and_then(|cmd| spawn(script_title(interp, code), cmd, Some(&script)));
    match started {
        Ok(handle) => Ok(handle),
        Err(e) => {
            std::fs::remove_file(&script)?;
            Err(e)
        }
    }
}

fn script_title(interp: Interpreter, code: &str) -> String {
    let first = code.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("");
    let mut short: String = first.chars().take(40).collect();
    if first.chars().count() > 40 {
        short.push('…');
    }
    format!("{}: {}", interp.label(), short)
}

/// Runs a script file with the interpreter for its extension; the working directory is the file's folder.
pub fn run_file(path: &Path) -> Result<RunHandle, ToolError> {
    let ext = path.extension().map(|e| e.to_string_lossy().to_string()).unwrap_or_default();
    let interp =
        Interpreter::from_extension(&ext).ok_or_else(|| ToolError::NotFound(format!("interpreter for .{ext}")))?;
    let cwd = path.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
    let title = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    spawn(title, interp.command(path, cwd)?, None)
}

static SCRIPT_COUNTER: AtomicU64 = AtomicU64::new(0);

fn write_temp_script(interp: Interpreter, code: &str) -> Result<PathBuf, ToolError> {
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.subsec_nanos()).unwrap_or(0);
    let name = format!(
        "kladde-{}-{}-{nanos}.{}",
        std::process::id(),
        SCRIPT_COUNTER.fetch_add(1, Ordering::Relaxed),
        interp.script_extension()
    );
    let path = std::env::temp_dir().join(name);
    let mut file = OpenOptions::new().write(true).create_new(true).open(&path)?;
    file.write_all(&interp.script_text(code))?;
    Ok(path)
}

/// Spawns `cmd` with piped output; reader threads and a waiter thread feed the handle's channel.
pub(crate) fn spawn(title: String, mut cmd: Command, script: Option<&Path>) -> Result<RunHandle, ToolError> {
    cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    hide_window(&mut cmd);
    ProcessTree::prepare(&mut cmd);
    let mut child = cmd.spawn()?;
    let tree = ProcessTree::attach(&child).or_else(|e| {
        child.kill()?;
        child.wait()?;
        Err(e)
    })?;
    let (tx, rx) = mpsc::channel();
    let (done_tx, done_rx) = mpsc::channel::<()>();
    let stdout = child.stdout.take().expect("stdout was configured as piped");
    let stderr = child.stderr.take().expect("stderr was configured as piped");
    spawn_reader(stdout, tx.clone(), done_tx.clone(), RunEvent::Stdout);
    spawn_reader(stderr, tx.clone(), done_tx, RunEvent::Stderr);
    let running = Arc::new(AtomicBool::new(true));
    let waiter_running = Arc::clone(&running);
    let waiter_script = script.map(Path::to_path_buf);
    thread::spawn(move || {
        let status = child.wait();
        waiter_running.store(false, Ordering::SeqCst);
        let deadline = Instant::now() + DRAIN_TIMEOUT;
        // Readers never send; this ends on disconnect or deadline.
        while done_rx.recv_timeout(deadline.saturating_duration_since(Instant::now())).is_ok() {}
        let mut events = Vec::new();
        if let Some(path) = waiter_script {
            if let Err(e) = std::fs::remove_file(&path) {
                events.push(RunEvent::Stderr(format!("could not remove {}: {e}", path.display())));
            }
        }
        match status {
            Ok(status) => events.push(RunEvent::Exited(status.code())),
            Err(e) => {
                events.push(RunEvent::Stderr(format!("could not wait for process: {e}")));
                events.push(RunEvent::Exited(None));
            }
        }
        // A send error means the handle was dropped: nobody listens.
        events.into_iter().try_for_each(|ev| tx.send(ev)).ok();
    });
    Ok(RunHandle { title, tree, events: rx, running, script: script.map(Path::to_path_buf) })
}

/// Forwards `source` line by line; `done` is dropped when the stream ends.
fn spawn_reader(
    source: impl Read + Send + 'static,
    tx: Sender<RunEvent>,
    done: Sender<()>,
    make: fn(String) -> RunEvent,
) {
    thread::spawn(move || {
        let mut reader = BufReader::new(source);
        let mut buf = Vec::new();
        let mut open = true;
        // Keep draining when nobody listens so the child never blocks.
        let mut listening = true;
        while open {
            buf.clear();
            let event = match reader.read_until(b'\n', &mut buf) {
                Ok(0) => {
                    open = false;
                    None
                }
                Ok(_) => {
                    let line = String::from_utf8_lossy(&buf);
                    Some(make(line.trim_end_matches(['\n', '\r']).to_string()))
                }
                Err(e) => {
                    open = false;
                    Some(RunEvent::Stderr(format!("could not read process output: {e}")))
                }
            };
            if let (true, Some(ev)) = (listening, event) {
                listening = tx.send(ev).is_ok();
            }
        }
        drop(done);
    });
}

/// Runs `code` in a new terminal window that stays open afterwards. The temp script deletes itself.
pub fn run_in_terminal(interp: Interpreter, code: &str, cwd: &Path) -> Result<(), ToolError> {
    let program = interp.require_program()?;
    let script = write_temp_script(interp, code)?;
    match open_terminal(interp, &program, &script, cwd) {
        Ok(()) => Ok(()),
        Err(e) => {
            std::fs::remove_file(&script)?;
            Err(e)
        }
    }
}

#[cfg(windows)]
fn open_terminal(interp: Interpreter, program: &Path, script: &Path, cwd: &Path) -> Result<(), ToolError> {
    use std::os::windows::process::CommandExt;
    let comspec = cmd_program().ok_or_else(|| ToolError::NotFound("cmd.exe".to_string()))?;
    let s = script.display();
    // Quote parity: each "&" is quoted for the outer cmd but not for
    // the console's own shell, so only the latter splits commands there.
    let target = if interp == Interpreter::PowerShell {
        let quoted = script.to_string_lossy().replace('\'', "''");
        format!(
            "\"{}\" -NoProfile -ExecutionPolicy Bypass -NoExit -Command \
             \"try {{ & '{quoted}' }} finally {{ Remove-Item -LiteralPath '{quoted}' }}\"",
            program.display()
        )
    } else {
        let invoke = match interp {
            Interpreter::Cmd => format!("call \"{s}\""),
            Interpreter::Bash => format!("\"{}\" \"{}\"", program.display(), shell_path(script).to_string_lossy()),
            _ => format!("\"{}\" \"{s}\"", program.display()),
        };
        format!("\"{}\" /D /S /K \"{invoke} & del \"{s}\"\"", comspec.display())
    };
    let mut cmd = crate::hidden_command(&comspec);
    // "start" opens a console; null stdio keeps our pipes out of it.
    cmd.args(["/D", "/S", "/C"])
        .raw_arg(format!("\"start \"Kladde\" /D \"{}\" {target}\"", cwd.display()))
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let status = cmd.status()?;
    if status.success() {
        Ok(())
    } else {
        Err(ToolError::Failed(format!("could not open a console window ({status})")))
    }
}

#[cfg(not(windows))]
fn open_terminal(interp: Interpreter, program: &Path, script: &Path, cwd: &Path) -> Result<(), ToolError> {
    const TERMINALS: [(&str, &str); 5] = [
        ("x-terminal-emulator", "-e"),
        ("gnome-terminal", "--"),
        ("konsole", "-e"),
        ("xfce4-terminal", "-x"),
        ("xterm", "-e"),
    ];
    let (terminal, exec_flag) = TERMINALS
        .iter()
        .find_map(|(name, flag)| find_program(name).map(|p| (p, *flag)))
        .ok_or_else(|| ToolError::NotFound("terminal emulator".to_string()))?;
    let wrapper = "cd -- \"$1\"; script=\"$2\"; shift 2; \"$@\"; rm -f -- \"$script\"; exec \"${SHELL:-/bin/sh}\"";
    let mut argv: Vec<OsString> = vec![terminal.into_os_string(), exec_flag.into(), "sh".into(), "-c".into()];
    argv.extend([wrapper.into(), "kladde".into(), cwd.as_os_str().into(), script.as_os_str().into()]);
    argv.push(program.as_os_str().into());
    argv.extend(interp.script_args(script));
    crate::spawn_detached(&argv, Some(cwd))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fence(lang: &str, header_line: usize, content: Range<usize>, closed: bool) -> FenceBlock {
        FenceBlock { lang: lang.to_string(), header_line, content, closed }
    }

    #[test]
    fn fences_backtick_and_tilde_with_info_strings() {
        let text = "intro\n```PowerShell title=x\nGet-Date\n```\n~~~ py\nprint(1)\n~~~\n";
        let blocks = find_fences(text);
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0], fence("powershell", 1, 28..37, true));
        assert_eq!(&text[blocks[0].content.clone()], "Get-Date\n");
        assert_eq!(blocks[1].lang, "py");
        assert_eq!(&text[blocks[1].content.clone()], "print(1)\n");
    }

    #[test]
    fn fences_other_marker_inside_is_body() {
        let text = "````md\n~~~\n```bash\nls\n```\n````\n~~~sh\n```\necho\n~~~";
        let blocks = find_fences(text);
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0].lang, "md");
        assert_eq!(&text[blocks[0].content.clone()], "~~~\n```bash\nls\n```\n");
        assert_eq!(blocks[1].lang, "sh");
        assert_eq!(&text[blocks[1].content.clone()], "```\necho\n");
        assert!(blocks[1].closed);
    }

    #[test]
    fn fences_closer_needs_same_marker_and_length() {
        let text = "````python\n```\nx = 1\n````\n";
        let blocks = find_fences(text);
        assert_eq!(blocks.len(), 1);
        assert_eq!(&text[blocks[0].content.clone()], "```\nx = 1\n");
        // A closing fence must not carry an info string.
        let blocks = find_fences("```js\n``` js\nalert(1)\n");
        assert_eq!(blocks.len(), 1);
        assert!(!blocks[0].closed);
    }

    #[test]
    fn fences_unclosed_runs_to_end() {
        let text = "```bash\r\necho hi\r\n";
        let blocks = find_fences(text);
        assert_eq!(blocks, vec![fence("bash", 0, 9..text.len(), false)]);
        assert_eq!(find_fences("```r"), vec![fence("r", 0, 4..4, false)]);
    }

    #[test]
    fn fences_quarto_braces_and_crlf() {
        let text = "---\r\ntitle: t\r\n---\r\n```{python}\r\n1+1\r\n```\r\n\
                    ```{r, echo=FALSE}\r\nx\r\n```\r\n```{.Python}\r\n```";
        let blocks = find_fences(text);
        let langs: Vec<&str> = blocks.iter().map(|b| b.lang.as_str()).collect();
        assert_eq!(langs, ["python", "r", "python"]);
        assert_eq!(&text[blocks[0].content.clone()], "1+1\r\n");
        assert_eq!(blocks[0].header_line, 3);
        assert!(blocks.iter().all(|b| b.closed));
        assert!(blocks[2].content.is_empty());
    }

    #[test]
    fn fences_rejects_short_indented_and_inline() {
        assert!(find_fences("``not a fence``\n``\ncode\n``\n").is_empty());
        assert!(find_fences("    ```bash\n    ls\n    ```\n").is_empty());
        assert!(find_fences("```a`b\nx\n```\n").len() == 1, "only the trailing bare fence opens a block");
        let blocks = find_fences("   ```\nplain\n   ```\n");
        assert_eq!(blocks, vec![fence("", 0, 7..13, true)]);
    }

    #[test]
    fn interpreter_from_lang() {
        assert_eq!(Interpreter::from_lang("PowerShell"), Some(Interpreter::PowerShell));
        assert_eq!(Interpreter::from_lang("pwsh"), Some(Interpreter::PowerShell));
        assert_eq!(Interpreter::from_lang("ps1"), Some(Interpreter::PowerShell));
        assert_eq!(Interpreter::from_lang("batch"), Some(Interpreter::Cmd));
        assert_eq!(Interpreter::from_lang("zsh"), Some(Interpreter::Bash));
        assert_eq!(Interpreter::from_lang("shell"), Some(Interpreter::Bash));
        assert_eq!(Interpreter::from_lang("python3"), Some(Interpreter::Python));
        assert_eq!(Interpreter::from_lang("javascript"), Some(Interpreter::Node));
        assert_eq!(Interpreter::from_lang("R"), Some(Interpreter::R));
        assert_eq!(Interpreter::from_lang("rust"), None);
        assert_eq!(Interpreter::from_lang(""), None);
    }

    #[test]
    fn interpreter_from_extension() {
        assert_eq!(Interpreter::from_extension("ps1"), Some(Interpreter::PowerShell));
        assert_eq!(Interpreter::from_extension(".BAT"), Some(Interpreter::Cmd));
        assert_eq!(Interpreter::from_extension("cmd"), Some(Interpreter::Cmd));
        assert_eq!(Interpreter::from_extension("sh"), Some(Interpreter::Bash));
        assert_eq!(Interpreter::from_extension("py"), Some(Interpreter::Python));
        assert_eq!(Interpreter::from_extension("js"), Some(Interpreter::Node));
        assert_eq!(Interpreter::from_extension("R"), Some(Interpreter::R));
        assert_eq!(Interpreter::from_extension("txt"), None);
        assert_eq!(Interpreter::from_extension(""), None);
    }

    #[cfg(windows)]
    #[test]
    fn windows_bash_is_git_bash_not_wsl() {
        let system = std::env::var("SystemRoot").unwrap_or_else(|_| "C:\\Windows".to_string()).to_lowercase();
        let bash = Interpreter::Bash.program();
        assert!(bash.as_ref().map_or(true, |p| !p.to_string_lossy().to_lowercase().starts_with(&system)), "{bash:?}");
        if find_program("git").is_some() {
            assert!(bash.is_some(), "git is installed, so Git Bash must be found");
        }
    }

    /// Collects events until `Exited` arrives, failing after `limit`.
    fn collect_until_exit(handle: &RunHandle, limit: Duration) -> Vec<RunEvent> {
        let start = Instant::now();
        let mut events = Vec::new();
        while !events.iter().any(|e| matches!(e, RunEvent::Exited(_))) {
            assert!(start.elapsed() < limit, "process did not exit in time: {events:?}");
            thread::sleep(Duration::from_millis(20));
            events.extend(handle.poll());
        }
        events
    }

    fn platform_shell() -> (Interpreter, &'static str) {
        if cfg!(windows) {
            (Interpreter::PowerShell, "Write-Output 'hello ä'\n[Console]::Error.WriteLine('oops')\nexit 3")
        } else {
            (Interpreter::Bash, "echo 'hello ä'\necho oops >&2\nexit 3")
        }
    }

    #[test]
    fn run_script_streams_output_and_exit_code() {
        let (interp, code) = platform_shell();
        let dir = tempfile::tempdir().expect("temp dir");
        let handle = run_script(interp, code, dir.path()).expect("script starts");
        let events = collect_until_exit(&handle, Duration::from_secs(60));
        assert!(events.contains(&RunEvent::Stdout("hello ä".to_string())), "{events:?}");
        assert!(events.contains(&RunEvent::Stderr("oops".to_string())), "{events:?}");
        assert_eq!(events.last(), Some(&RunEvent::Exited(Some(3))));
        assert!(!handle.is_running());
        assert!(!handle.script.as_ref().expect("temp script").exists());
    }

    #[test]
    fn run_script_uses_cwd() {
        let dir = tempfile::tempdir().expect("temp dir");
        std::fs::write(dir.path().join("marker.txt"), "x").expect("write marker");
        let (interp, code) = if cfg!(windows) {
            (Interpreter::Cmd, "@if exist marker.txt (echo found) else (echo missing)")
        } else {
            (Interpreter::Bash, "test -e marker.txt && echo found || echo missing")
        };
        let handle = run_script(interp, code, dir.path()).expect("script starts");
        let events = collect_until_exit(&handle, Duration::from_secs(60));
        assert!(events.contains(&RunEvent::Stdout("found".to_string())), "{events:?}");
        assert_eq!(events.last(), Some(&RunEvent::Exited(Some(0))));
    }

    /// Starts `code`, waits for "started", kills, and checks that a grandchild printing "late" died too.
    fn assert_kill_stops_tree(interp: Interpreter, code: &str) {
        let dir = tempfile::tempdir().expect("temp dir");
        let handle = run_script(interp, code, dir.path()).expect("script starts");
        let start = Instant::now();
        let mut events = Vec::new();
        while !events.contains(&RunEvent::Stdout("started".to_string())) {
            assert!(start.elapsed() < Duration::from_secs(60), "no output: {events:?}");
            thread::sleep(Duration::from_millis(20));
            events.extend(handle.poll());
        }
        assert!(handle.is_running());
        handle.kill().expect("kill succeeds");
        let events = collect_until_exit(&handle, Duration::from_secs(10));
        assert!(matches!(events.last(), Some(RunEvent::Exited(code)) if *code != Some(0)), "{events:?}");
        assert!(!handle.script.as_ref().expect("temp script").exists());
        thread::sleep(Duration::from_secs(4));
        let late = handle.poll();
        assert!(!late.contains(&RunEvent::Stdout("late".to_string())), "grandchild survived: {late:?}");
        handle.kill().expect("killing a finished process is a no-op");
    }

    #[test]
    fn kill_stops_bash_and_its_children() {
        if Interpreter::Bash.program().is_none() {
            println!("bash not installed; skipping kill test");
        } else {
            assert_kill_stops_tree(Interpreter::Bash, "(sleep 2; echo late) &\necho started\nsleep 60");
        }
    }

    #[cfg(windows)]
    #[test]
    fn kill_stops_powershell_and_its_children() {
        let code = "$p = Start-Process cmd -ArgumentList '/c ping -n 3 127.0.0.1 >nul & echo late' -NoNewWindow \
                    -PassThru\nWrite-Output started\nStart-Sleep -Seconds 60";
        assert_kill_stops_tree(Interpreter::PowerShell, code);
    }

    #[test]
    fn run_file_picks_interpreter_by_extension() {
        let dir = tempfile::tempdir().expect("temp dir");
        let (name, code) = if cfg!(windows) { ("t.cmd", "@echo ran %CD%\r\n") } else { ("t.sh", "echo ran $PWD\n") };
        let path = dir.path().join(name);
        std::fs::write(&path, code).expect("write script");
        let handle = run_file(&path).expect("file runs");
        let events = collect_until_exit(&handle, Duration::from_secs(60));
        let cwd = std::fs::canonicalize(dir.path()).expect("canonical dir");
        let ran = events.iter().any(|e| match e {
            RunEvent::Stdout(line) => {
                line.strip_prefix("ran ").is_some_and(|d| std::fs::canonicalize(d).is_ok_and(|d| d == cwd))
            }
            _ => false,
        });
        assert!(ran, "{events:?}");
        assert!(path.exists(), "run_file must not delete the user's file");
        assert!(matches!(run_file(&dir.path().join("x.unknown")), Err(ToolError::NotFound(_))));
    }
}
