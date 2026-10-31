//! The only place in this crate that calls the kernel.
//!
//! `getattrlistbulk(2)` fills a caller-owned buffer with as many directory
//! entries as fit. The buffer is allocated as `Vec<u64>` so the kernel always gets
//! an 8-byte-aligned pointer (`FILEID` and `timespec` values arrive at 4-byte
//! offsets, but the syscall's own requirement is documented as 4-byte alignment).
//! Bytes are handed to [`crate::parse`] as a plain slice; the parser does the rest
//! without any `unsafe`.
//!
//! # Safety
//!
//! Every `unsafe` block below is a single syscall or an allocation-to-slice view.
//! Both explain their invariant in a `// SAFETY:` comment. No pointer returned by
//! the kernel is ever dereferenced.

use std::fs::File;
use std::io;
use std::mem::size_of;
use std::os::fd::AsRawFd;
use std::path::Path;

use crate::attrlist::Request;
use crate::parse::{parse_record, RawEntry};

/// Bytes in the per-directory buffer.
///
/// Large enough that a typical directory is covered by one or two syscalls, small
/// enough that one buffer per worker stays cache-friendly. `getattrlistbulk`
/// returns whole entries only, so a bigger buffer does not risk partial records.
pub const BUFFER_BYTES: usize = 256 * 1024;

const BUFFER_WORDS: usize = BUFFER_BYTES / size_of::<u64>();

/// Streams the entries of one directory.
///
/// The iterator yields one [`RawEntry`] per `getattrlistbulk` record. Each `next()`
/// call costs nothing once a buffer is loaded; a refill is one syscall. I/O and
/// structural errors are yielded as `Err` from the iterator (the error is sticky:
/// after it, iteration stops).
///
/// Directory order is whatever the filesystem returns. The scanner sorts where it
/// needs determinism; the parser does not reorder.
pub struct DirReader {
    file: File,
    attr_list: libc::attrlist,
    buffer: Vec<u64>,
    /// Byte offset of the next unconsumed record in `buffer`.
    cursor: usize,
    /// Records left in the current buffer, as reported by the kernel.
    pending: usize,
    /// Set after the kernel says there are no more entries, or after an error.
    finished: bool,
    request: Request,
}

impl DirReader {
    /// Open `path` as a directory and request [`Request::storage`] attributes.
    pub fn open(path: impl AsRef<Path>) -> io::Result<Self> {
        Self::with_request(path, Request::storage())
    }

    /// Open `path` with a custom attribute request.
    ///
    /// Only useful for tests: the parser knows how to size the storage request's
    /// attributes, and rejects any other bit it encounters.
    pub fn with_request(path: impl AsRef<Path>, request: Request) -> io::Result<Self> {
        let file = File::open(path)?;
        Ok(Self {
            file,
            attr_list: attr_list_for(request),
            buffer: vec![0; BUFFER_WORDS],
            cursor: 0,
            pending: 0,
            finished: false,
            request,
        })
    }

    /// The request this reader was built with.
    pub fn request(&self) -> Request {
        self.request
    }

    /// Ask the kernel for the next batch.
    ///
    /// Returns the number of records written; `0` means the directory is done.
    fn fill(&mut self) -> io::Result<usize> {
        let size = self.buffer.len() * size_of::<u64>();
        // SAFETY: `self.buffer` is a live allocation of `BUFFER_WORDS` u64s, so
        // `as_mut_ptr()` is non-null and 8-byte aligned for `size` bytes;
        // `self.attr_list` is an initialised `libc::attrlist`. getattrlistbulk only
        // writes into those two objects and does not retain the pointers.
        let count = unsafe {
            libc::getattrlistbulk(
                self.file.as_raw_fd(),
                std::ptr::from_mut(&mut self.attr_list).cast(),
                self.buffer.as_mut_ptr().cast(),
                size,
                Request::options(),
            )
        };
        if count < 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(count as usize)
        }
    }

    /// View the word buffer as bytes.
    fn buffer_bytes(&self) -> &[u8] {
        let len = self.buffer.len() * size_of::<u64>();
        // SAFETY: every bit pattern is a valid `u8`, the pointer is non-null and
        // valid for `len` readable bytes, and the borrow is tied to `self`, so the
        // slice cannot outlive the allocation.
        unsafe { std::slice::from_raw_parts(self.buffer.as_ptr().cast::<u8>(), len) }
    }
}

impl Iterator for DirReader {
    type Item = io::Result<RawEntry>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.pending == 0 {
            if self.finished {
                return None;
            }
            match self.fill() {
                Ok(0) => {
                    self.finished = true;
                    return None;
                }
                Ok(count) => {
                    self.pending = count;
                    self.cursor = 0;
                }
                Err(error) => {
                    self.finished = true;
                    return Some(Err(error));
                }
            }
        }

        let mut cursor = self.cursor;
        let parsed = parse_record(self.buffer_bytes(), &mut cursor, &self.request)
            .map(|entry| (entry, cursor))
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error));

        self.pending -= 1;
        match parsed {
            Ok((entry, cursor)) => {
                self.cursor = cursor;
                Some(Ok(entry))
            }
            Err(error) => {
                // A malformed record leaves no trustworthy way to find the next
                // one, so the directory is abandoned rather than resynchronised.
                self.finished = true;
                self.pending = 0;
                Some(Err(error))
            }
        }
    }
}

// --- I/O throttling ---------------------------------------------------------

/// `IOPOL_TYPE_DISK` from `<sys/resource.h>`.
const IOPOL_TYPE_DISK: libc::c_int = 0;
/// `IOPOL_SCOPE_THREAD`: the policy applies to the calling thread only.
const IOPOL_SCOPE_THREAD: libc::c_int = 1;
/// `IOPOL_THROTTLE`: background I/O, so the machine stays responsive.
const IOPOL_THROTTLE: libc::c_int = 3;

extern "C" {
    fn setiopolicy_np(iotype: libc::c_int, scope: libc::c_int, policy: libc::c_int) -> libc::c_int;
}

thread_local! {
    static IO_POLICY_APPLIED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Throttle disk I/O for the current thread, once.
///
/// Rayon reuses worker threads across directories and scans, so the first task on
/// each thread applies the policy and later tasks skip the syscall. Failure is
/// ignored: throttling is a responsiveness nicety, not a correctness requirement.
pub(crate) fn throttle_current_thread() {
    IO_POLICY_APPLIED.with(|applied| {
        if applied.get() {
            return;
        }
        applied.set(true);
        // SAFETY: `setiopolicy_np` only changes the calling thread's I/O policy and
        // takes three integer arguments from <sys/resource.h>; it reads and writes
        // nothing else.
        unsafe {
            setiopolicy_np(IOPOL_TYPE_DISK, IOPOL_SCOPE_THREAD, IOPOL_THROTTLE);
        }
    });
}

/// Build the `struct attrlist` the kernel expects.
///
/// `libc::attrlist` already has the correct layout; re-declaring it here would
/// invite a silent ABI drift on a future SDK.
fn attr_list_for(request: Request) -> libc::attrlist {
    libc::attrlist {
        bitmapcount: libc::ATTR_BIT_MAP_COUNT,
        reserved: 0,
        commonattr: request.common,
        // Volume attributes are rejected by getattrlistbulk, and fork attributes
        // are not needed to account for disk usage.
        volattr: 0,
        dirattr: request.dir,
        fileattr: request.file,
        forkattr: 0,
    }
}
