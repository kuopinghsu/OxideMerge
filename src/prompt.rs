//! Asking on the terminal what a server asks while signing in (see
//! [`crate::core::remote::set_asker`]): a one-time code, a key's passphrase. The questions go
//! to the terminal itself, not standard output, so they neither end up in a redirected report
//! nor wait on input that a pipe gives; without a terminal (a scheduled job, say), every
//! question is declined.

use crate::core::remote::Challenge;
use std::sync::atomic::AtomicBool;

/// Asks `challenge` on the terminal, an answer whose question is not to be echoed typed
/// without showing it. What the server wrote is shown without the control characters that
/// would make the terminal do something else than show it.
pub fn ask(challenge: &Challenge, _cancel: &AtomicBool) -> Option<Vec<String>> {
    let (mut input, mut output) = terminal()?;
    let mut heading = shown(&challenge.server);
    if !challenge.name.trim().is_empty() {
        heading = format!("{heading}: {}", shown(challenge.name.trim()));
    }
    show(&mut output, &format!("{heading}\n"))?;
    if !challenge.instructions.trim().is_empty() {
        show(
            &mut output,
            &format!("{}\n", shown(challenge.instructions.trim())),
        )?;
    }
    let mut answers = Vec::with_capacity(challenge.prompts.len());
    for prompt in &challenge.prompts {
        show(&mut output, &shown(&prompt.text))?;
        let hidden = (!prompt.echo).then(|| Hidden::new(&input));
        let line = read_line(&mut input);
        drop(hidden);
        if !prompt.echo {
            show(&mut output, "\n")?;
        }
        answers.push(line?);
    }
    Some(answers)
}

/// `text` from a server without control characters other than line breaks and tabs: no
/// escape sequences to move the cursor, rewrite the title or the clipboard, or hide what is
/// asked.
fn shown(text: &str) -> String {
    text.chars()
        .filter(|&c| !c.is_control() || c == '\n' || c == '\t')
        .collect()
}

/// Writes `text` to the terminal.
#[cfg(not(windows))]
fn show(output: &mut std::fs::File, text: &str) -> Option<()> {
    use std::io::Write;
    output.write_all(text.as_bytes()).ok()?;
    output.flush().ok()
}

/// Writes `text` to the console as UTF-16, which shows it whatever the console's code page.
#[cfg(windows)]
fn show(output: &mut std::fs::File, text: &str) -> Option<()> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::System::Console::WriteConsoleW;
    let handle = output.as_raw_handle() as windows_sys::Win32::Foundation::HANDLE;
    let units: Vec<u16> = text.encode_utf16().collect();
    let mut done = 0;
    while done < units.len() {
        let mut written = 0u32;
        let count = (units.len() - done).min(8192) as u32;
        // SAFETY: `handle` is the open console output, and `units[done..]` holds `count`
        // units at least.
        let ok = unsafe {
            WriteConsoleW(
                handle,
                units[done..].as_ptr().cast(),
                count,
                &mut written,
                std::ptr::null(),
            )
        };
        if ok == 0 || written == 0 {
            return None;
        }
        done += written as usize;
    }
    Some(())
}

/// One line typed on the terminal, without its line ending; `None` at the end of input.
#[cfg(not(windows))]
fn read_line(input: &mut std::fs::File) -> Option<String> {
    read_line_from(input)
}

/// One line read from `input` without its line ending, `None` at its end. Read a byte at a
/// time, so that no buffer is left holding what was typed.
#[cfg_attr(windows, allow(dead_code))]
fn read_line_from(input: &mut impl std::io::Read) -> Option<String> {
    let mut bytes = Vec::with_capacity(256);
    let mut byte = [0u8; 1];
    let ended = loop {
        match input.read(&mut byte) {
            Ok(0) => break bytes.is_empty(),
            Ok(_) if byte[0] == b'\n' => break false,
            Ok(_) => push_wiping(&mut bytes, byte[0]),
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(_) => break true,
        }
    };
    wipe_all(&mut byte);
    if bytes.last() == Some(&b'\r') {
        bytes.pop();
    }
    let line = String::from_utf8_lossy(&bytes).into_owned();
    wipe_all(&mut bytes);
    finish(line, ended)
}

/// Pushes `x` onto `v`, wiping what `v` held when it has to move to make room, so that no copy
/// of what was typed is left in freed memory.
fn push_wiping<T: Copy + Default>(v: &mut Vec<T>, x: T) {
    if v.len() == v.capacity() {
        let mut bigger = Vec::with_capacity((v.capacity() * 2).max(256));
        bigger.extend_from_slice(v);
        wipe_all(v);
        *v = bigger;
    }
    v.push(x);
}

/// Overwrites every element of `v`, in a way that is not left out as having no effect.
fn wipe_all<T: Copy + Default>(v: &mut [T]) {
    for x in v.iter_mut() {
        // SAFETY: `x` is a valid, aligned reference into `v`.
        unsafe { std::ptr::write_volatile(x, T::default()) };
    }
}

/// One line typed on the console, read as UTF-16 so that an answer outside the console's
/// code page arrives as typed; `None` at the end of input.
#[cfg(windows)]
fn read_line(input: &mut std::fs::File) -> Option<String> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::System::Console::ReadConsoleW;
    let handle = input.as_raw_handle() as windows_sys::Win32::Foundation::HANDLE;
    let mut units: Vec<u16> = Vec::with_capacity(256);
    let mut unit = [0u16; 1];
    let ended = loop {
        let mut read = 0u32;
        // SAFETY: `handle` is the open console input, and `unit` has room for the one unit
        // asked for.
        let ok = unsafe {
            ReadConsoleW(
                handle,
                unit.as_mut_ptr().cast(),
                1,
                &mut read,
                std::ptr::null(),
            )
        };
        if ok == 0 {
            break true;
        }
        if read == 0 {
            break units.is_empty();
        }
        if unit[0] == u16::from(b'\n') {
            break false;
        }
        push_wiping(&mut units, unit[0]);
    };
    wipe_all(&mut unit);
    if units.last() == Some(&u16::from(b'\r')) {
        units.pop();
    }
    // Sized for the longest it can come out, so that it never moves while being filled.
    let mut line = String::with_capacity(units.len() * 3);
    for c in char::decode_utf16(units.iter().copied()) {
        line.push(c.unwrap_or(char::REPLACEMENT_CHARACTER));
    }
    wipe_all(&mut units);
    finish(line, ended)
}

/// `line`, unless input `ended` before it did, when it is wiped instead.
fn finish(mut line: String, ended: bool) -> Option<String> {
    if ended {
        crate::core::remote::wipe(&mut line);
        return None;
    }
    Some(line)
}

/// The terminal, to read from and to write to.
#[cfg(unix)]
fn terminal() -> Option<(std::fs::File, std::fs::File)> {
    let tty = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/tty")
        .ok()?;
    let out = tty.try_clone().ok()?;
    Some((tty, out))
}

#[cfg(windows)]
fn terminal() -> Option<(std::fs::File, std::fs::File)> {
    let input = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open("CONIN$")
        .ok()?;
    let output = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open("CONOUT$")
        .ok()?;
    Some((input, output))
}

/// The terminal's echo turned off for as long as this lives, and turned back on whenever a
/// signal stops or ends the program meanwhile (`Ctrl+C`, `Ctrl+Z`), which would otherwise
/// leave it off; off again once the program goes on.
struct Hidden {
    /// The terminal and its settings.
    #[cfg(unix)]
    restore: Option<(i32, libc::termios)>,
    #[cfg(windows)]
    restore: Option<(windows_sys::Win32::Foundation::HANDLE, u32)>,
}

/// What a signal handler restores the terminal from, as `readpassphrase` does: the terminal,
/// its settings with and without echo, and the handlers in place before.
#[cfg(unix)]
mod on_signal {
    use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};

    /// The signals handled: those that end the program, and those that stop it.
    pub(super) const SIGNALS: [i32; 7] = [
        libc::SIGINT,
        libc::SIGTERM,
        libc::SIGHUP,
        libc::SIGQUIT,
        libc::SIGTSTP,
        libc::SIGTTIN,
        libc::SIGTTOU,
    ];

    /// The terminal to restore, or -1; set only once the rest is.
    pub(super) static FD: AtomicI32 = AtomicI32::new(-1);
    /// Whether each of [`SIGNALS`] has the handler, its earlier one in [`STATE`].
    pub(super) static INSTALLED: [AtomicBool; 7] = [const { AtomicBool::new(false) }; 7];

    pub(super) struct State {
        pub(super) echoing: libc::termios,
        pub(super) quiet: libc::termios,
        pub(super) previous: [libc::sigaction; 7],
    }

    pub(super) struct Shared(std::cell::UnsafeCell<State>);
    // SAFETY: written only while `FD` is -1 and no handler is installed, by the one thread
    // asking, and read only by the handler, installed after the write.
    unsafe impl Sync for Shared {}
    // SAFETY: all of it is plain data, for which zeroes are a valid value.
    pub(super) static STATE: Shared =
        Shared(std::cell::UnsafeCell::new(unsafe { std::mem::zeroed() }));

    impl Shared {
        /// Written to only while no handler is installed.
        pub(super) fn get(&self) -> *mut State {
            self.0.get()
        }
    }

    /// The signals blocked while the handler runs: all of [`SIGNALS`], so that setting the
    /// terminal from the background does not stop it again.
    pub(super) fn mask() -> libc::sigset_t {
        // SAFETY: `set` is plain data that `sigemptyset` initializes.
        unsafe {
            let mut set: libc::sigset_t = std::mem::zeroed();
            libc::sigemptyset(&mut set);
            for signal in SIGNALS {
                libc::sigaddset(&mut set, signal);
            }
            set
        }
    }

    /// Restores the terminal, lets the signal do what it did before (end or stop the program,
    /// or call the handler there was), and, should the program go on, hides the echo again.
    pub(super) extern "C" fn handle(signal: i32) {
        let Some(i) = SIGNALS.iter().position(|&s| s == signal) else {
            return;
        };
        // SAFETY: only async-signal-safe calls, on settings written before the handler was
        // installed.
        unsafe {
            let state = &*STATE.get();
            let fd = FD.load(Ordering::SeqCst);
            if fd >= 0 {
                libc::tcsetattr(fd, libc::TCSAFLUSH, &state.echoing);
            }
            let mut ours: libc::sigaction = std::mem::zeroed();
            libc::sigaction(signal, &state.previous[i], &mut ours);
            let mut only: libc::sigset_t = std::mem::zeroed();
            libc::sigemptyset(&mut only);
            libc::sigaddset(&mut only, signal);
            libc::pthread_sigmask(libc::SIG_UNBLOCK, &only, std::ptr::null_mut());
            libc::raise(signal);
            libc::pthread_sigmask(libc::SIG_BLOCK, &only, std::ptr::null_mut());
            if FD.load(Ordering::SeqCst) >= 0 {
                libc::sigaction(signal, &ours, std::ptr::null_mut());
                libc::tcsetattr(fd, libc::TCSAFLUSH, &state.quiet);
            }
        }
    }
}

impl Hidden {
    #[cfg(unix)]
    fn new(tty: &std::fs::File) -> Self {
        use std::os::unix::io::AsRawFd;
        use std::sync::atomic::Ordering;
        let fd = tty.as_raw_fd();
        let installed = &on_signal::INSTALLED;
        // SAFETY: `fd` is an open terminal for the calls, `termios` and `sigaction` are plain
        // data the calls fill in before they are read, and the state the handler reads is
        // written before it is installed.
        let restore = unsafe {
            let mut echoing: libc::termios = std::mem::zeroed();
            if libc::tcgetattr(fd, &mut echoing) != 0 {
                return Hidden { restore: None };
            }
            let mut quiet = echoing;
            quiet.c_lflag &= !libc::ECHO;
            let state = on_signal::STATE.get();
            (*state).echoing = echoing;
            (*state).quiet = quiet;
            for (i, signal) in on_signal::SIGNALS.into_iter().enumerate() {
                let mut previous: libc::sigaction = std::mem::zeroed();
                // A signal ignored, as under `nohup`, stays ignored.
                if libc::sigaction(signal, std::ptr::null(), &mut previous) != 0
                    || previous.sa_sigaction == libc::SIG_IGN
                {
                    continue;
                }
                (*state).previous[i] = previous;
                let mut action: libc::sigaction = std::mem::zeroed();
                action.sa_sigaction = on_signal::handle as extern "C" fn(i32) as usize;
                action.sa_mask = on_signal::mask();
                if libc::sigaction(signal, &action, std::ptr::null_mut()) == 0 {
                    installed[i].store(true, Ordering::SeqCst);
                }
            }
            on_signal::FD.store(fd, Ordering::SeqCst);
            let set = loop {
                if libc::tcsetattr(fd, libc::TCSAFLUSH, &quiet) == 0 {
                    break true;
                }
                if std::io::Error::last_os_error().kind() != std::io::ErrorKind::Interrupted {
                    break false;
                }
            };
            set.then_some((fd, echoing))
        };
        let hidden = Hidden { restore };
        if hidden.restore.is_none() {
            Self::uninstall();
        }
        hidden
    }

    /// Puts back the handlers there were, and stops restoring the terminal on a signal.
    #[cfg(unix)]
    fn uninstall() {
        use std::sync::atomic::Ordering;
        on_signal::FD.store(-1, Ordering::SeqCst);
        for (i, signal) in on_signal::SIGNALS.into_iter().enumerate() {
            if on_signal::INSTALLED[i].swap(false, Ordering::SeqCst) {
                // SAFETY: puts back the handler read when installing this one.
                unsafe {
                    let previous = &(*on_signal::STATE.get()).previous[i];
                    libc::sigaction(signal, previous, std::ptr::null_mut());
                }
            }
        }
    }

    #[cfg(windows)]
    fn new(console: &std::fs::File) -> Self {
        use std::os::windows::io::AsRawHandle;
        use std::sync::atomic::Ordering;
        use windows_sys::Win32::System::Console::{
            GetConsoleMode, SetConsoleCtrlHandler, SetConsoleMode, ENABLE_ECHO_INPUT,
        };
        let handle = console.as_raw_handle() as windows_sys::Win32::Foundation::HANDLE;
        let mut old = 0u32;
        // SAFETY: `handle` is the open console input for the calls, `old` a valid place for
        // the mode, and what the handler reads is stored before it is added.
        let restore = unsafe {
            if GetConsoleMode(handle, &mut old) == 0 {
                return Hidden { restore: None };
            }
            on_ctrl::MODE.store(old, Ordering::SeqCst);
            on_ctrl::HANDLE.store(handle, Ordering::SeqCst);
            let added = SetConsoleCtrlHandler(Some(on_ctrl::handle), 1) != 0;
            if SetConsoleMode(handle, old & !ENABLE_ECHO_INPUT) != 0 {
                Some((handle, old))
            } else {
                if added {
                    SetConsoleCtrlHandler(Some(on_ctrl::handle), 0);
                }
                on_ctrl::HANDLE.store(std::ptr::null_mut(), Ordering::SeqCst);
                None
            }
        };
        Hidden { restore }
    }
}

/// What turns the console's echo back on when `Ctrl+C`, `Ctrl+Break` or closing the console
/// ends the program while it is off, which would otherwise leave it off.
#[cfg(windows)]
mod on_ctrl {
    use std::sync::atomic::{AtomicPtr, AtomicU32, Ordering};

    /// The console input to restore, or null.
    pub(super) static HANDLE: AtomicPtr<std::ffi::c_void> = AtomicPtr::new(std::ptr::null_mut());
    /// Its mode with echo.
    pub(super) static MODE: AtomicU32 = AtomicU32::new(0);

    /// Restores the console's mode, then leaves the event to the handlers there were before,
    /// which end the program.
    pub(super) unsafe extern "system" fn handle(
        _event: u32,
    ) -> windows_sys::Win32::Foundation::BOOL {
        let handle = HANDLE.load(Ordering::SeqCst);
        if !handle.is_null() {
            // SAFETY: the console input stays open for as long as the handle is set.
            unsafe {
                windows_sys::Win32::System::Console::SetConsoleMode(
                    handle,
                    MODE.load(Ordering::SeqCst),
                )
            };
        }
        0
    }
}

impl Drop for Hidden {
    fn drop(&mut self) {
        #[cfg(unix)]
        if let Some((fd, echoing)) = &self.restore {
            Self::uninstall();
            // SAFETY: restores the settings read from the same, still open, terminal.
            unsafe { libc::tcsetattr(*fd, libc::TCSAFLUSH, echoing) };
        }
        #[cfg(windows)]
        if let Some((handle, old)) = self.restore {
            use windows_sys::Win32::System::Console::{SetConsoleCtrlHandler, SetConsoleMode};
            // SAFETY: removes the handler added with this, then restores the mode read from
            // the same, still open, console.
            unsafe {
                SetConsoleCtrlHandler(Some(on_ctrl::handle), 0);
                on_ctrl::HANDLE.store(std::ptr::null_mut(), std::sync::atomic::Ordering::SeqCst);
                SetConsoleMode(handle, old);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_line_is_read_without_its_ending_and_the_end_of_input_declines() {
        let mut input: &[u8] = b"123456\r\nsecond\nlast";
        assert_eq!(read_line_from(&mut input).as_deref(), Some("123456"));
        assert_eq!(read_line_from(&mut input).as_deref(), Some("second"));
        assert_eq!(read_line_from(&mut input).as_deref(), Some("last"));
        assert_eq!(read_line_from(&mut input), None);

        let long = "x".repeat(1000);
        let mut input = format!("{long}\n").into_bytes();
        assert_eq!(read_line_from(&mut input.as_slice()), Some(long));
        input.clear();
    }

    #[test]
    fn what_a_server_writes_is_shown_without_terminal_controls() {
        assert_eq!(
            shown("Code\x1b]52;c;ZXZpbA==\x07 for\u{9b}2J bank:\tnow\r\nnext"),
            "Code]52;c;ZXZpbA== for2J bank:\tnow\nnext"
        );
    }

    /// Set for the copy of the tests that [`ctrl_c_at_a_hidden_answer_turns_echo_back_on`]
    /// runs on a terminal of its own.
    #[cfg(unix)]
    const ON_TERMINAL: &str = "OXIDEMERGE_PROMPT_ON_TERMINAL";

    /// Asks one hidden question, in that copy only.
    #[test]
    #[cfg(unix)]
    fn asks_a_hidden_question_on_a_terminal_of_its_own() {
        if std::env::var_os(ON_TERMINAL).is_none() {
            return;
        }
        let challenge = Challenge {
            server: "h:22".into(),
            name: String::new(),
            instructions: String::new(),
            prompts: vec![crate::core::remote::Prompt {
                text: "Code: ".into(),
                echo: false,
            }],
        };
        let _ = ask(&challenge, &AtomicBool::new(false));
    }

    /// A pseudo-terminal, both ends.
    #[cfg(unix)]
    fn pty() -> (std::fs::File, std::fs::File) {
        use std::os::unix::io::FromRawFd;
        let (mut m, mut s) = (-1, -1);
        // SAFETY: `m` and `s` receive the two new descriptors; the rest may be null.
        let made = unsafe {
            libc::openpty(
                &mut m,
                &mut s,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        };
        assert_eq!(made, 0, "{}", std::io::Error::last_os_error());
        // SAFETY: both were just opened, and are owned here alone.
        unsafe { (std::fs::File::from_raw_fd(m), std::fs::File::from_raw_fd(s)) }
    }

    /// Whether `tty` echoes; read through the master side, which shares the settings and,
    /// unlike the other, stays usable once the child that made it its terminal is gone.
    #[cfg(unix)]
    fn echoing(tty: &std::fs::File) -> bool {
        use std::os::unix::io::AsRawFd;
        // SAFETY: `tty` is an open terminal, and `tcgetattr` fills in all of `t`.
        unsafe {
            let mut t: libc::termios = std::mem::zeroed();
            assert_eq!(libc::tcgetattr(tty.as_raw_fd(), &mut t), 0);
            t.c_lflag & libc::ECHO != 0
        }
    }

    #[test]
    #[cfg(unix)]
    fn ctrl_c_at_a_hidden_answer_turns_echo_back_on() {
        use std::io::{Read, Write};
        use std::os::unix::process::{CommandExt, ExitStatusExt};
        use std::time::{Duration, Instant};
        let (mut master, slave) = pty();
        assert!(echoing(&master));
        let mut command = std::process::Command::new(std::env::current_exe().unwrap());
        command
            .args([
                "--exact",
                "prompt::tests::asks_a_hidden_question_on_a_terminal_of_its_own",
            ])
            .env(ON_TERMINAL, "1")
            .stdin(slave.try_clone().unwrap())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        // SAFETY: only async-signal-safe calls: the terminal on standard input made the
        // child's own, so that it opens as `/dev/tty` and `Ctrl+C` there interrupts it, and
        // hang-ups ignored, as under `nohup`.
        unsafe {
            command.pre_exec(|| {
                if libc::setsid() < 0 || libc::ioctl(0, libc::TIOCSCTTY as _, 0) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                libc::signal(libc::SIGHUP, libc::SIG_IGN);
                Ok(())
            });
        }
        let mut child = command.spawn().unwrap();
        let start = Instant::now();
        let wait_for = |what: &str, done: &dyn Fn() -> bool| {
            while !done() {
                assert!(start.elapsed() < Duration::from_secs(30), "{what}");
                std::thread::sleep(Duration::from_millis(10));
            }
        };

        let mut seen = Vec::new();
        while !String::from_utf8_lossy(&seen).contains("Code: ") {
            assert!(start.elapsed() < Duration::from_secs(30), "no question");
            let mut buf = [0u8; 256];
            let n = master.read(&mut buf).unwrap();
            seen.extend_from_slice(&buf[..n]);
        }
        // The question is shown just before the echo is turned off.
        wait_for("echo off", &|| !echoing(&master));

        // A hang-up ignored before stays ignored.
        // SAFETY: signals the child, still running.
        unsafe { libc::kill(child.id() as i32, libc::SIGHUP) };
        std::thread::sleep(Duration::from_millis(200));
        assert!(child.try_wait().unwrap().is_none(), "a hang-up ended it");
        assert!(!echoing(&master));

        master.write_all(b"12\x03").unwrap();
        let status = child.wait().unwrap();
        assert_eq!(status.signal(), Some(libc::SIGINT), "{status:?}");
        assert!(echoing(&master), "echo is back on");
    }
}
