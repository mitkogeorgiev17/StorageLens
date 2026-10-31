//! Elevation state and the "relaunch elevated" flow.
//!
//! Unelevated, parts of `C:\` are unreadable. PLAN §7.3: report them as "not
//! scanned" — never silently under-report — and offer to relaunch elevated. The
//! scanner records [`is_elevated`] in `ScanResult::elevated`, so the UI can tell
//! "these folders were denied because the scan was not elevated" apart from
//! "denied even though it was".
//!
//! # Relaunching
//!
//! [`relaunch_elevated`] starts the current executable again through the `runas`
//! verb, which shows the UAC prompt, with the original arguments plus the scan
//! options. The caller exits once it returns `Ok`; the elevated instance reads
//! its options back with [`options_from_args`] and starts the same scan.
//!
//! The options travel as hex-encoded JSON in one argument. Hex has no quotes,
//! spaces, or backslashes, so it survives Windows command-line parsing untouched
//! whatever the exclude paths contain.
//!
//! Everything except [`is_elevated`] and [`relaunch_elevated`] is pure and
//! unit-tested on every runner.

use std::fmt::Write as _;
use std::io;

use storagelens_core::ScanOptions;

/// The argument that carries the scan options into the elevated instance.
pub const SCAN_OPTIONS_FLAG: &str = "--scan-options";

/// Encode options for the command line: JSON, then lowercase hex.
pub fn encode_options(options: &ScanOptions) -> io::Result<String> {
    let json = serde_json::to_vec(options).map_err(io::Error::other)?;
    let mut hex = String::with_capacity(json.len() * 2);
    for byte in json {
        // Writing to a String cannot fail.
        let _ = write!(hex, "{byte:02x}");
    }
    Ok(hex)
}

/// Decode options produced by [`encode_options`].
pub fn decode_options(hex: &str) -> io::Result<ScanOptions> {
    let invalid = |message: &str| io::Error::new(io::ErrorKind::InvalidInput, message.to_owned());
    if hex.len() % 2 != 0 {
        return Err(invalid("scan options have an odd number of hex digits"));
    }
    let bytes = hex
        .as_bytes()
        .chunks_exact(2)
        .map(|pair| {
            std::str::from_utf8(pair)
                .ok()
                .and_then(|digits| u8::from_str_radix(digits, 16).ok())
                .ok_or_else(|| invalid("scan options are not hex"))
        })
        .collect::<io::Result<Vec<u8>>>()?;
    serde_json::from_slice(&bytes).map_err(|error| invalid(&error.to_string()))
}

/// The scan options passed by a relaunch, if any.
///
/// `args` excludes the program name. A malformed value is an error rather than a
/// silent fallback to defaults: scanning with the wrong excludes is worse than
/// not scanning.
pub fn options_from_args(args: &[String]) -> io::Result<Option<ScanOptions>> {
    let mut found = None;
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        if arg == SCAN_OPTIONS_FLAG {
            let value = iter.next().ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidInput, "--scan-options needs a value")
            })?;
            found = Some(decode_options(value)?);
        }
    }
    Ok(found)
}

/// The arguments for the elevated instance: the current ones minus any earlier
/// `--scan-options`, plus the new options.
pub fn relaunch_args(current: &[String], options: &ScanOptions) -> io::Result<Vec<String>> {
    let mut out = Vec::with_capacity(current.len() + 2);
    let mut iter = current.iter();
    while let Some(arg) = iter.next() {
        if arg == SCAN_OPTIONS_FLAG {
            let _ = iter.next();
        } else {
            out.push(arg.clone());
        }
    }
    out.push(SCAN_OPTIONS_FLAG.to_owned());
    out.push(encode_options(options)?);
    Ok(out)
}

/// Quote one argument so `CommandLineToArgvW` and the MSVC runtime read it back
/// unchanged.
///
/// Backslashes are literal except before a quote, so a run of `n` backslashes
/// before a quote (or before the closing quote) becomes `2n`.
pub fn quote_arg(arg: &str) -> String {
    if !arg.is_empty() && !arg.contains([' ', '\t', '\n', '\x0b', '"']) {
        return arg.to_owned();
    }

    let mut out = String::with_capacity(arg.len() + 2);
    out.push('"');
    let mut backslashes = 0usize;
    for ch in arg.chars() {
        match ch {
            '\\' => backslashes += 1,
            '"' => {
                out.extend(std::iter::repeat_n('\\', backslashes * 2 + 1));
                out.push('"');
                backslashes = 0;
            }
            _ => {
                out.extend(std::iter::repeat_n('\\', backslashes));
                out.push(ch);
                backslashes = 0;
            }
        }
    }
    out.extend(std::iter::repeat_n('\\', backslashes * 2));
    out.push('"');
    out
}

/// Join quoted arguments into one command-line string.
pub fn command_line(args: &[String]) -> String {
    args.iter()
        .map(|arg| quote_arg(arg))
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(windows)]
pub use sys::{is_elevated, relaunch_elevated};

#[cfg(windows)]
mod sys {
    use std::io;
    use std::os::windows::ffi::OsStrExt;

    use storagelens_core::ScanOptions;
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::{CloseHandle, HANDLE};
    use windows::Win32::Security::{
        GetTokenInformation, TokenElevation, TOKEN_ELEVATION, TOKEN_QUERY,
    };
    use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
    use windows::Win32::UI::Shell::ShellExecuteW;
    use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

    use super::{command_line, relaunch_args};

    fn wide_c(text: &std::ffi::OsStr) -> Vec<u16> {
        text.encode_wide().chain(std::iter::once(0)).collect()
    }

    /// Whether this process runs with an elevated (administrator) token.
    ///
    /// `false` when the token cannot be queried: assuming elevation would hide
    /// the "relaunch elevated" offer exactly when it might help.
    pub fn is_elevated() -> bool {
        let mut token = HANDLE::default();
        // SAFETY: the pseudo-handle from `GetCurrentProcess` is always valid, and
        // `token` is writable.
        if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) }.is_err() {
            return false;
        }

        let mut elevation = TOKEN_ELEVATION::default();
        let mut returned = 0u32;
        // SAFETY: `token` was opened with TOKEN_QUERY above; `elevation` is a
        // writable TOKEN_ELEVATION and its exact size is passed.
        let queried = unsafe {
            GetTokenInformation(
                token,
                TokenElevation,
                Some((&mut elevation as *mut TOKEN_ELEVATION).cast()),
                std::mem::size_of::<TOKEN_ELEVATION>() as u32,
                &mut returned,
            )
        };
        // SAFETY: `token` is a real handle from `OpenProcessToken`, closed once.
        let _ = unsafe { CloseHandle(token) };

        queried.is_ok() && elevation.TokenIsElevated != 0
    }

    /// Start this executable again, elevated, carrying `options`.
    ///
    /// Shows the UAC prompt. `Ok` means the elevated instance was launched; the
    /// caller should then exit. Declining the prompt is an error
    /// (`ERROR_CANCELLED`), and the current process keeps running unelevated.
    pub fn relaunch_elevated(options: &ScanOptions) -> io::Result<()> {
        let exe = std::env::current_exe()?;
        let current = std::env::args_os()
            .skip(1)
            .map(|arg| {
                arg.into_string().map_err(|_| {
                    io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "an argument is not valid Unicode and cannot be passed on",
                    )
                })
            })
            .collect::<io::Result<Vec<String>>>()?;
        let parameters = command_line(&relaunch_args(&current, options)?);

        let verb = wide_c("runas".as_ref());
        let file = wide_c(exe.as_os_str());
        let parameters = wide_c(parameters.as_ref());

        // SAFETY: every string is NUL-terminated and outlives the call; a null
        // window and directory are allowed.
        let instance = unsafe {
            ShellExecuteW(
                None,
                PCWSTR(verb.as_ptr()),
                PCWSTR(file.as_ptr()),
                PCWSTR(parameters.as_ptr()),
                PCWSTR::null(),
                SW_SHOWNORMAL,
            )
        };

        // ShellExecuteW reports success as a value greater than 32.
        if instance.0 as usize > 32 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    fn options() -> ScanOptions {
        ScanOptions {
            skip_system: false,
            cross_filesystems: true,
            threads: 3,
            exclude: vec![
                PathBuf::from(r"C:\Program Files\My App"),
                PathBuf::from(r#"D:\odd "quoted" \dir\"#),
                PathBuf::from(r"E:\Фото"),
            ],
            max_depth: Some(7),
        }
    }

    #[test]
    fn options_round_trip_through_hex() {
        let hex = encode_options(&options()).expect("encode");
        assert!(hex.bytes().all(|byte| byte.is_ascii_hexdigit()));
        assert_eq!(decode_options(&hex).expect("decode"), options());
    }

    #[test]
    fn malformed_options_are_errors() {
        assert!(decode_options("abc").is_err(), "odd length");
        assert!(decode_options("zz").is_err(), "not hex");
        assert!(decode_options("7b7d").is_err(), "{{}} is missing fields");
    }

    #[test]
    fn relaunch_replaces_old_options_and_keeps_other_args() {
        let old = encode_options(&ScanOptions::default()).expect("encode");
        let current = vec![
            "--verbose".to_owned(),
            SCAN_OPTIONS_FLAG.to_owned(),
            old,
            r"C:\Users\me".to_owned(),
        ];
        let args = relaunch_args(&current, &options()).expect("args");

        assert_eq!(&args[..2], ["--verbose", r"C:\Users\me"]);
        assert_eq!(
            args.iter().filter(|arg| *arg == SCAN_OPTIONS_FLAG).count(),
            1
        );
        assert_eq!(
            options_from_args(&args).expect("parse"),
            Some(options()),
            "the elevated instance gets exactly the options it was given"
        );
    }

    #[test]
    fn missing_options_are_none_and_a_dangling_flag_is_an_error() {
        assert_eq!(options_from_args(&["x".to_owned()]).expect("parse"), None);
        assert!(options_from_args(&[SCAN_OPTIONS_FLAG.to_owned()]).is_err());
    }

    #[test]
    fn arguments_are_quoted_for_command_line_to_argv() {
        let cases = [
            ("plain", "plain"),
            ("", r#""""#),
            ("has space", r#""has space""#),
            (r"C:\dir\", r"C:\dir\"),
            (r"C:\my dir\", r#""C:\my dir\\""#),
            (r#"say "hi""#, r#""say \"hi\"""#),
            (r#"a\"b"#, r#""a\\\"b""#),
            (r"a\\b c", r#""a\\b c""#),
        ];
        for (arg, quoted) in cases {
            assert_eq!(quote_arg(arg), quoted, "quoting {arg:?}");
        }
        assert_eq!(
            command_line(&["a b".to_owned(), "c".to_owned()]),
            r#""a b" c"#
        );
    }

    /// GitHub's Windows runners run elevated; this only checks the query works
    /// and agrees with what `net session` (admin-only) reports.
    #[cfg(windows)]
    #[test]
    fn elevation_matches_the_admin_only_net_session_probe() {
        let admin = std::process::Command::new("net")
            .arg("session")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|status| status.success())
            .unwrap_or(false);
        assert_eq!(is_elevated(), admin);
    }
}
