//! Reading RAR archives with the UnRAR library (`unrar_sys`, its C interface built from
//! source). The library only decodes here: it is asked to test each file, and the data it
//! hands over while it does is written by this module, so that nothing lands outside the
//! file opened for it, no link is ever created, and what is written can be counted.

use std::ffi::c_int;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};
use unrar_sys as sys;
use zeroize::Zeroizing;

/// Why a RAR archive could not be read.
#[derive(Debug)]
pub(super) enum Error {
    /// A password is needed, or the one given is wrong.
    NeedsPassword,
    /// The archive is one of several volumes, which are not read.
    Volumes,
    /// A file unpacks to more than was allowed for it.
    TooLarge,
    /// Data that does not check out: a damaged archive, or, for the older format, which
    /// cannot tell, a wrong password.
    BadData,
    Write(std::io::Error),
    Other(&'static str),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::NeedsPassword => f.write_str("a password is needed"),
            Error::Volumes => f.write_str("RAR archives in several volumes are not supported"),
            Error::TooLarge => f.write_str("archive too large"),
            Error::BadData => f.write_str("the archive is damaged"),
            Error::Write(e) => e.fmt(f),
            Error::Other(text) => f.write_str(text),
        }
    }
}

/// What an entry stands in for instead of holding data of its own.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Link {
    /// A symbolic link or a junction.
    Symbolic,
    /// Another entry of the archive with the same contents: a hard link, or a copy stored
    /// once. Its name in the archive.
    SameAs(PathBuf),
}

/// An entry of a RAR archive.
#[derive(Debug)]
pub(super) struct Header {
    pub name: PathBuf,
    pub dir: bool,
    pub encrypted: bool,
    /// Whether the entry goes on in, or comes from, another volume.
    pub split: bool,
    pub size: u64,
    pub modified: Option<SystemTime>,
    pub link: Option<Link>,
}

/// What the library's callback and the [`Reader`] share.
struct State {
    /// The password, as the wide string the library asks for.
    password: Option<Zeroizing<Vec<sys::WCHAR>>>,
    /// Set once the library asked for a password there is none of.
    asked: bool,
    /// Set once the library asked for another volume.
    volumes: bool,
    /// Where the data of the file being read goes, and how much of it there may be.
    out: Option<std::fs::File>,
    left: u64,
    over: bool,
    failed: Option<std::io::Error>,
}

/// A RAR archive open for reading its entries one after another: [`Reader::next`], then
/// [`Reader::skip`] or [`Reader::extract`] for each.
pub(super) struct Reader {
    handle: *const sys::Handle,
    state: *mut State,
    names_encrypted: bool,
}

impl Drop for Reader {
    fn drop(&mut self) {
        // SAFETY: `handle` is null or what `RAROpenArchiveEx` returned, closed only here;
        // `state` came from `Box::into_raw` in `open` and the library, the only other user
        // of it, is done with it once the archive is closed.
        unsafe {
            if !self.handle.is_null() {
                sys::RARCloseArchive(self.handle);
            }
            drop(Box::from_raw(self.state));
        }
    }
}

/// `text` as the wide characters of the C library (UTF-16 on Windows, UTF-32 elsewhere),
/// without an ending.
fn wide(text: &str) -> Vec<sys::WCHAR> {
    #[cfg(windows)]
    return text.encode_utf16().collect();
    #[cfg(not(windows))]
    return text.chars().map(|c| c as u32 as sys::WCHAR).collect();
}

/// The text of the wide characters in `buf`, up to its ending.
fn from_wide(buf: &[sys::WCHAR]) -> PathBuf {
    let buf = &buf[..buf.iter().position(|&c| c == 0).unwrap_or(buf.len())];
    #[cfg(windows)]
    return String::from_utf16_lossy(buf).into();
    #[cfg(not(windows))]
    return buf
        .iter()
        .map(|&c| char::from_u32(c as u32).unwrap_or(char::REPLACEMENT_CHARACTER))
        .collect::<String>()
        .into();
}

/// The name of an entry. Linux builds of the library convert names with the C locale,
/// which a Rust program leaves unset, so there the bytes are taken as they are.
fn entry_name(header: &sys::HeaderDataEx) -> PathBuf {
    #[cfg(any(target_os = "linux", target_os = "netbsd"))]
    {
        use std::os::unix::ffi::OsStrExt;
        let bytes: Vec<u8> = header
            .filename
            .iter()
            .take_while(|&&c| c != 0)
            .map(|&c| c as u8)
            .collect();
        std::ffi::OsStr::from_bytes(&bytes).into()
    }
    #[cfg(not(any(target_os = "linux", target_os = "netbsd")))]
    from_wide(&header.filename_w)
}

/// A time of the library (100 ns steps since 1601) as a system time; none for zero.
fn file_time(low: u32, high: u32) -> Option<SystemTime> {
    const TO_1970: u64 = 116_444_736_000_000_000;
    let ticks = (u64::from(high) << 32 | u64::from(low)).checked_sub(TO_1970)?;
    SystemTime::UNIX_EPOCH.checked_add(Duration::from_nanos(ticks.checked_mul(100)?))
}

/// Called by the library while it reads: for a password, for the next volume, and with
/// each piece of a file's data.
extern "C" fn callback(
    msg: sys::UINT,
    user: sys::LPARAM,
    p1: sys::LPARAM,
    p2: sys::LPARAM,
) -> c_int {
    let run = || -> c_int {
        // SAFETY: `user` is the `State` given to `RAROpenArchiveEx`, which outlives the
        // archive's handle, and nothing else touches it while the library runs.
        let Some(state) = (unsafe { (user as *mut State).as_mut() }) else {
            return -1;
        };
        match msg {
            // Another volume is never opened: the answer stops the library.
            sys::UCM_CHANGEVOLUME | sys::UCM_CHANGEVOLUMEW => {
                state.volumes = true;
                -1
            }
            sys::UCM_NEEDPASSWORDW => {
                let (Some(password), true) = (&state.password, p1 != 0 && p2 > 0) else {
                    state.asked = true;
                    return -1;
                };
                // SAFETY: the library passes a buffer of `p2` wide characters at `p1`.
                let buf =
                    unsafe { std::slice::from_raw_parts_mut(p1 as *mut sys::WCHAR, p2 as usize) };
                let n = password.len().min(buf.len() - 1);
                buf[..n].copy_from_slice(&password[..n]);
                buf[n] = 0;
                1
            }
            // Asked when the wide one gave nothing.
            sys::UCM_NEEDPASSWORD => {
                state.asked = true;
                -1
            }
            sys::UCM_PROCESSDATA => {
                let Some(out) = state.out.as_mut() else {
                    return 1;
                };
                if p1 == 0 || p2 <= 0 {
                    return 1;
                }
                // SAFETY: the library passes `p2` bytes of unpacked data at `p1`.
                let data = unsafe { std::slice::from_raw_parts(p1 as *const u8, p2 as usize) };
                let Some(left) = state.left.checked_sub(data.len() as u64) else {
                    state.over = true;
                    return -1;
                };
                state.left = left;
                match out.write_all(data) {
                    Ok(()) => 1,
                    Err(e) => {
                        state.failed = Some(e);
                        -1
                    }
                }
            }
            _ => 0,
        }
    };
    // A panic must not cross into the library.
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(run)).unwrap_or(-1)
}

impl Reader {
    /// Opens the archive at `path`, to be read with `password` where it is encrypted.
    pub fn open(path: &Path, password: Option<&str>) -> Result<Reader, Error> {
        let state = Box::into_raw(Box::new(State {
            password: password.map(|p| Zeroizing::new(wide(p))),
            asked: false,
            volumes: false,
            out: None,
            left: 0,
            over: false,
            failed: None,
        }));
        // The library takes the name as bytes on Linux (see `entry_name`), wide elsewhere.
        #[cfg(any(target_os = "linux", target_os = "netbsd"))]
        let (narrow, wide_name) = {
            use std::os::unix::ffi::OsStrExt;
            let name = std::ffi::CString::new(path.as_os_str().as_bytes());
            (name.ok(), None::<Vec<sys::WCHAR>>)
        };
        #[cfg(not(any(target_os = "linux", target_os = "netbsd")))]
        let (narrow, wide_name) = {
            let mut name = wide(&path.to_string_lossy());
            let whole = !name.contains(&0);
            name.push(0);
            (None::<std::ffi::CString>, whole.then_some(name))
        };
        let mut reader = Reader {
            handle: std::ptr::null(),
            state,
            names_encrypted: false,
        };
        if narrow.is_none() && wide_name.is_none() {
            return Err(Error::Other("the name of the archive cannot be used"));
        }
        let mut data = sys::OpenArchiveDataEx {
            archive_name: narrow.as_ref().map_or(std::ptr::null(), |n| n.as_ptr()),
            archive_name_w: wide_name.as_ref().map_or(std::ptr::null(), |n| n.as_ptr()),
            open_mode: sys::RAR_OM_EXTRACT,
            open_result: 0,
            comment_buffer: std::ptr::null_mut(),
            comment_buffer_size: 0,
            comment_size: 0,
            comment_state: 0,
            flags: 0,
            callback: Some(callback),
            user_data: state as sys::LPARAM,
            op_flags: 0,
            comment_buffer_w: std::ptr::null_mut(),
            reserved: [0; 25],
        };
        // SAFETY: `data` is filled as the library documents, the names it points to live
        // until the call returns, and `state` lives as long as the handle (see `Drop`).
        reader.handle = unsafe { sys::RAROpenArchiveEx(&mut data as *mut _ as *const _) };
        let result = data.open_result as c_int;
        if reader.handle.is_null() || result != sys::ERAR_SUCCESS {
            return Err(reader.error(result));
        }
        if data.flags & (sys::ROADF_VOLUME | sys::ROADF_FIRSTVOLUME) != 0 {
            return Err(Error::Volumes);
        }
        reader.names_encrypted = data.flags & sys::ROADF_ENCHEADERS != 0;
        Ok(reader)
    }

    /// Whether the names of the archive's entries are encrypted along with their data.
    pub fn names_encrypted(&self) -> bool {
        self.names_encrypted
    }

    fn state(&mut self) -> &mut State {
        // SAFETY: `state` is valid until `drop`, and the library only uses it during a
        // call into it, which no caller of this is in.
        unsafe { &mut *self.state }
    }

    /// The error for the result `code` of a call into the library that failed.
    fn error(&mut self, code: c_int) -> Error {
        let state = self.state();
        if state.volumes {
            return Error::Volumes;
        }
        if state.over {
            return Error::TooLarge;
        }
        if let Some(e) = state.failed.take() {
            return Error::Write(e);
        }
        match code {
            sys::ERAR_MISSING_PASSWORD | sys::ERAR_BAD_PASSWORD => Error::NeedsPassword,
            _ if state.asked => Error::NeedsPassword,
            sys::ERAR_BAD_DATA => Error::BadData,
            sys::ERAR_BAD_ARCHIVE | sys::ERAR_UNKNOWN_FORMAT => Error::Other("not a RAR archive"),
            sys::ERAR_EOPEN => Error::Other("the archive could not be opened"),
            sys::ERAR_NO_MEMORY => Error::Other("not enough memory"),
            sys::ERAR_EREAD => Error::Other("the archive could not be read"),
            _ => Error::Other("the archive could not be unpacked"),
        }
    }

    /// The next entry of the archive, to be skipped or extracted before the one after it;
    /// none at the end.
    #[allow(clippy::should_implement_trait)]
    pub fn next(&mut self) -> Result<Option<Header>, Error> {
        let mut link: Vec<sys::WCHAR> = vec![0; 2048];
        let mut header = Box::<sys::HeaderDataEx>::default();
        header.redir_name = link.as_mut_ptr();
        header.redir_name_size = link.len() as std::ffi::c_uint;
        // SAFETY: `handle` is an open archive, and `header` is the structure the library
        // fills, with `redir_name` a buffer of the size given with it.
        let code = unsafe { sys::RARReadHeaderEx(self.handle, &mut *header as *mut _ as *const _) };
        match code {
            sys::ERAR_SUCCESS => {}
            sys::ERAR_END_ARCHIVE if !self.state().volumes => return Ok(None),
            _ => return Err(self.error(code)),
        }
        // What the library names the kinds of links: 4 is a hard link, 5 a file stored once.
        let link = match header.redir_type {
            0 => None,
            4 | 5 => Some(Link::SameAs(from_wide(&link))),
            _ => Some(Link::Symbolic),
        };
        Ok(Some(Header {
            name: entry_name(&header),
            dir: header.flags & sys::RHDF_DIRECTORY != 0,
            encrypted: header.flags & sys::RHDF_ENCRYPTED != 0,
            split: header.flags & (sys::RHDF_SPLITBEFORE | sys::RHDF_SPLITAFTER) != 0,
            size: u64::from(header.unp_size_high) << 32 | u64::from(header.unp_size),
            modified: file_time(header.mtime_low, header.mtime_high),
            link,
        }))
    }

    fn process(&mut self, operation: c_int) -> Result<(), Error> {
        let none = std::ptr::null();
        // SAFETY: `handle` is an open archive whose current entry `next` has just read; no
        // destination is given, as the library writes nothing itself.
        let code = unsafe { sys::RARProcessFileW(self.handle, operation, none, none) };
        match code {
            sys::ERAR_SUCCESS => Ok(()),
            _ => Err(self.error(code)),
        }
    }

    /// Passes over the entry [`Reader::next`] gave.
    pub fn skip(&mut self) -> Result<(), Error> {
        self.process(sys::RAR_SKIP)
    }

    /// Writes the data of the entry [`Reader::next`] gave to `file`, at most `limit` bytes
    /// of it, checking it against the archive's checksum. Returns how much was written.
    pub fn extract(&mut self, file: std::fs::File, limit: u64) -> Result<u64, Error> {
        let state = self.state();
        (state.out, state.left) = (Some(file), limit);
        let done = self.process(sys::RAR_TEST);
        let state = self.state();
        let flushed = state.out.take().map_or(Ok(()), |mut f| f.flush());
        done?;
        flushed.map_err(Error::Write)?;
        Ok(limit - self.state().left)
    }
}
