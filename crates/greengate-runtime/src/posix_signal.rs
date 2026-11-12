//! The raw POSIX calls a signal handler is built from, shared by the
//! handlers this crate installs: `staged_only`'s restore-then-die handler and
//! `cluster sign --watch`'s stop flag.
//!
//! Externs rather than a dependency: `scripts/check-no-deps.sh` keeps this
//! binary crate-free, and these are five libc calls with stable signatures —
//! `pipe`/`read`/`write` need nothing beyond plain integers and byte pointers,
//! so unlike `sigset_t`-based APIs there is no opaque, platform-varying struct
//! layout to get wrong by hand.

// Numeric literals rather than a `libc` import: unlike `sigset_t`-based APIs
// these four numbers are fixed by POSIX on every platform this runs on.
pub(crate) const SIGHUP: i32 = 1;
pub(crate) const SIGINT: i32 = 2;
pub(crate) const SIGQUIT: i32 = 3;
pub(crate) const SIGTERM: i32 = 15;
/// `signal(2)`'s default action.
pub(crate) const SIG_DFL: usize = 0;

extern "C" {
    #[link_name = "signal"]
    fn libc_signal_raw(sig: i32, handler: usize) -> usize;
    #[link_name = "raise"]
    fn libc_raise_raw(sig: i32) -> i32;
    #[link_name = "pipe"]
    fn libc_pipe_raw(fds: *mut i32) -> i32;
    #[link_name = "read"]
    fn libc_read_raw(fd: i32, buf: *mut u8, count: usize) -> isize;
    #[link_name = "write"]
    fn libc_write_raw(fd: i32, buf: *const u8, count: usize) -> isize;
}

/// `signal(2)`: `handler` is [`SIG_DFL`] or an `extern "C" fn(i32)` cast to
/// `usize`.
pub(crate) unsafe fn libc_signal(sig: i32, handler: usize) {
    unsafe {
        libc_signal_raw(sig, handler);
    }
}

pub(crate) unsafe fn libc_raise(sig: i32) {
    unsafe {
        libc_raise_raw(sig);
    }
}

pub(crate) unsafe fn libc_pipe(fds: *mut i32) -> i32 {
    unsafe { libc_pipe_raw(fds) }
}

pub(crate) unsafe fn libc_read(fd: i32, buf: *mut u8, count: usize) -> isize {
    unsafe { libc_read_raw(fd, buf, count) }
}

pub(crate) unsafe fn libc_write(fd: i32, buf: *const u8, count: usize) -> isize {
    unsafe { libc_write_raw(fd, buf, count) }
}
