//! Passwords and passphrases held in memory.
//!
//! A [`Secret`] owns whole memory pages of its own, so locking and unlocking
//! them never affects anything else. The pages are locked into RAM (`mlock`
//! on Unix, `VirtualLock` on Windows) so they are never written to swap, kept
//! out of core dumps on Linux (`MADV_DONTDUMP`), and overwritten with zeros
//! before they are released. Locking is best effort: if the OS refuses (a low
//! `RLIMIT_MEMLOCK`), the secret still works and is still wiped.
//!
//! The text field a password is typed into is ordinary memory owned by the
//! GUI toolkit; callers wipe what they control (see [`wipe_string`]).

use std::alloc::{Layout, alloc_zeroed, dealloc};
use std::sync::atomic::{Ordering, compiler_fence};

/// Overwrite `bytes` with zeros in a way the optimiser cannot remove.
pub fn wipe(bytes: &mut [u8]) {
    for b in bytes.iter_mut() {
        // SAFETY: `b` is a valid, exclusive reference to one byte.
        unsafe { std::ptr::write_volatile(b, 0) };
    }
    compiler_fence(Ordering::SeqCst);
}

/// Zero a `String`'s whole allocation (not just its length) and empty it.
pub fn wipe_string(s: &mut String) {
    // SAFETY: zero bytes are valid UTF-8, and the length is set to 0 before
    // the spare capacity is touched through the raw vector.
    let v = unsafe { s.as_mut_vec() };
    v.clear();
    let cap = v.capacity();
    // SAFETY: the allocation is `cap` bytes long and owned by `v`.
    wipe(unsafe { std::slice::from_raw_parts_mut(v.as_mut_ptr(), cap) });
}

/// The OS page size (allocations are rounded up to whole pages).
fn page_size() -> usize {
    #[cfg(unix)]
    {
        // SAFETY: sysconf has no preconditions.
        let n = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
        if n > 0 { n as usize } else { 4096 }
    }
    #[cfg(not(unix))]
    {
        4096
    }
}

#[cfg(windows)]
#[link(name = "kernel32")]
unsafe extern "system" {
    fn VirtualLock(addr: *mut std::ffi::c_void, size: usize) -> i32;
    fn VirtualUnlock(addr: *mut std::ffi::c_void, size: usize) -> i32;
}

/// A secret in locked, wiped-on-drop memory. Holds at most one page minus
/// one byte (4 KiB or more), far beyond any password.
pub struct Secret {
    ptr: *mut u8,
    len: usize,
    layout: Layout,
}

// SAFETY: `Secret` exclusively owns its allocation, like a `Box<[u8]>`.
unsafe impl Send for Secret {}
// SAFETY: shared access only reads.
unsafe impl Sync for Secret {}

impl Secret {
    /// Copy `bytes` into fresh locked pages; anything past the capacity is dropped.
    pub fn new(bytes: &[u8]) -> Self {
        let page = page_size();
        let layout = Layout::from_size_align(page, page).expect("page layout");
        // SAFETY: the layout has a non-zero size.
        let ptr = unsafe { alloc_zeroed(layout) };
        if ptr.is_null() {
            std::alloc::handle_alloc_error(layout);
        }
        // SAFETY: `ptr` is a live allocation of `page` bytes owned by us.
        unsafe {
            #[cfg(unix)]
            {
                libc::mlock(ptr.cast(), page);
                #[cfg(target_os = "linux")]
                libc::madvise(ptr.cast(), page, libc::MADV_DONTDUMP);
            }
            #[cfg(windows)]
            VirtualLock(ptr.cast(), page);
        }
        let len = bytes.len().min(page - 1);
        // SAFETY: `len` bytes fit in the allocation, which cannot overlap `bytes`.
        unsafe { std::ptr::copy_nonoverlapping(bytes.as_ptr(), ptr, len) };
        Self { ptr, len, layout }
    }

    /// Move a typed answer into a secret, wiping the `String` it came from.
    pub fn from_string(mut s: String) -> Self {
        let secret = Self::new(s.as_bytes());
        wipe_string(&mut s);
        secret
    }

    /// The secret's bytes. Do not copy them into ordinary memory.
    pub fn expose(&self) -> &[u8] {
        // SAFETY: the first `len` bytes of the allocation were initialised in `new`.
        unsafe { std::slice::from_raw_parts(self.ptr, self.len) }
    }
}

impl Clone for Secret {
    fn clone(&self) -> Self {
        Self::new(self.expose())
    }
}

impl Drop for Secret {
    fn drop(&mut self) {
        let size = self.layout.size();
        // SAFETY: the allocation is live, `size` bytes, and allocated with `layout`.
        unsafe {
            wipe(std::slice::from_raw_parts_mut(self.ptr, size));
            #[cfg(unix)]
            libc::munlock(self.ptr.cast(), size);
            #[cfg(windows)]
            VirtualUnlock(self.ptr.cast(), size);
            dealloc(self.ptr, self.layout);
        }
    }
}

impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Secret(***)")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secrets_hold_copy_and_wipe() {
        let s = Secret::from_string("hunter2".to_string());
        assert_eq!(s.expose(), b"hunter2");
        assert_eq!(s.clone().expose(), b"hunter2");
        assert_eq!(format!("{s:?}"), "Secret(***)");
        let huge = vec![b'x'; 1 << 20];
        assert_eq!(Secret::new(&huge).expose().len(), page_size() - 1);

        let mut t = String::with_capacity(32);
        t.push_str("password");
        let p = t.as_ptr();
        wipe_string(&mut t);
        assert!(t.is_empty());
        // SAFETY: `t` still owns its 32-byte allocation.
        assert!(unsafe { std::slice::from_raw_parts(p, 8) }.iter().all(|&b| b == 0));
    }
}
