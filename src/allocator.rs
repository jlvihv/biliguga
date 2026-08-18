#[cfg(all(target_os = "linux", target_env = "gnu"))]
mod jemalloc {
    use std::ffi::{c_char, c_int, c_void};

    #[repr(transparent)]
    pub(crate) struct Config(*const c_char);

    // The configuration has to be visible before jemalloc services the first
    // allocation, which happens before `main`. Exporting this conventional
    // jemalloc symbol avoids creating one arena per CPU group up front.
    unsafe impl Sync for Config {}

    #[unsafe(no_mangle)]
    pub(crate) static malloc_conf: Config =
        Config(c"narenas:2,dirty_decay_ms:0,muzzy_decay_ms:0,background_thread:false".as_ptr());

    #[link(name = "jemalloc")]
    unsafe extern "C" {
        fn mallctl(
            name: *const c_char,
            old: *mut c_void,
            old_len: *mut usize,
            new: *mut c_void,
            new_len: usize,
        ) -> c_int;
    }

    unsafe fn set_isize(name: &[u8], value: isize) {
        let mut value = value;
        unsafe {
            let _ = mallctl(
                name.as_ptr().cast(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                (&mut value as *mut isize).cast(),
                size_of::<isize>(),
            );
        }
    }

    fn arena_count() -> u32 {
        let mut count = 0_u32;
        let mut size = size_of::<u32>();
        unsafe {
            let _ = mallctl(
                c"arenas.narenas".as_ptr(),
                (&mut count as *mut u32).cast(),
                &mut size,
                std::ptr::null_mut(),
                0,
            );
        }
        count
    }

    pub(super) fn configure() {
        // FFmpeg creates many short-lived threads. Immediate decay keeps their
        // freed pages from becoming a per-video resident-memory high-water
        // mark. These defaults also apply to arenas created later.
        unsafe {
            set_isize(b"arenas.dirty_decay_ms\0", 0);
            set_isize(b"arenas.muzzy_decay_ms\0", 0);
        }
        for arena in 0..arena_count() {
            let dirty = format!("arena.{arena}.dirty_decay_ms\0");
            let muzzy = format!("arena.{arena}.muzzy_decay_ms\0");
            unsafe {
                set_isize(dirty.as_bytes(), 0);
                set_isize(muzzy.as_bytes(), 0);
            }
        }
    }

    pub(super) fn trim() {
        // libmpv has already destroyed the handle and joined decoder threads.
        // Purging here releases only pages that jemalloc now knows are free.
        for arena in 0..arena_count() {
            let command = format!("arena.{arena}.purge\0");
            unsafe {
                let _ = mallctl(
                    command.as_ptr().cast(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    0,
                );
            }
        }
    }
}

pub(crate) fn configure() {
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    jemalloc::configure();
}

pub(crate) fn trim() {
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    jemalloc::trim();
}
