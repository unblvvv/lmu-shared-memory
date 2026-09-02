use crate::{Error, Result};

#[cfg(windows)]
mod platform {
    use std::sync::atomic::{AtomicI32, Ordering};

    use super::*;
    use windows::{
        Win32::{
            Foundation::{CloseHandle, HANDLE, HWND, INVALID_HANDLE_VALUE},
            System::{
                Memory::{
                    CreateFileMappingW, FILE_MAP_ALL_ACCESS, FILE_MAP_READ,
                    MEMORY_MAPPED_VIEW_ADDRESS, MapViewOfFile, OpenFileMappingW, PAGE_READWRITE,
                    UnmapViewOfFile,
                },
                Threading::{CreateEventW, SetEvent},
            },
            UI::WindowsAndMessaging::IsWindow,
        },
        core::HSTRING,
    };

    const LOCK_MAPPING_NAME: &str = "LMU_SharedMemoryLockData";
    const LOCK_EVENT_NAME: &str = "LMU_SharedMemoryLockEvent";
    const LOCK_DATA_SIZE: usize = 8;
    const MAX_LOCK_SPINS: usize = 64;

    pub struct SharedMemory {
        name: String,
        handle: HANDLE,
        view: MEMORY_MAPPED_VIEW_ADDRESS,
        size: usize,
    }

    impl SharedMemory {
        pub fn open(name: &str, size: usize) -> Result<Self> {
            let name_wide = HSTRING::from(name);
            let handle = unsafe { OpenFileMappingW(FILE_MAP_READ.0, false, &name_wide) }.map_err(
                |error| Error::SharedMemoryUnavailable {
                    name: name.into(),
                    source: error.to_string(),
                },
            )?;
            let view = unsafe { MapViewOfFile(handle, FILE_MAP_READ, 0, 0, size) };
            if view.Value.is_null() {
                let _ = unsafe { CloseHandle(handle) };
                return Err(Error::MapViewFailed { name: name.into() });
            }
            Ok(Self {
                name: name.into(),
                handle,
                view,
                size,
            })
        }
        pub fn read<T: Copy>(&self, offset: usize) -> Result<T> {
            let value_size = std::mem::size_of::<T>();
            if offset
                .checked_add(value_size)
                .is_none_or(|end| end > self.size)
            {
                return Err(Error::OutOfBounds {
                    name: self.name.clone(),
                    offset,
                    value_size,
                    mapping_size: self.size,
                });
            }
            let pointer = unsafe { self.view.Value.cast::<u8>().add(offset).cast::<T>() };
            Ok(unsafe { pointer.read_unaligned() })
        }
        pub fn len(&self) -> usize {
            self.size
        }
        pub fn is_empty(&self) -> bool {
            self.size == 0
        }
    }
    impl Drop for SharedMemory {
        fn drop(&mut self) {
            let _ = unsafe { UnmapViewOfFile(self.view) };
            let _ = unsafe { CloseHandle(self.handle) };
        }
    }
    unsafe impl Send for SharedMemory {}
    unsafe impl Sync for SharedMemory {}

    pub fn source_window_alive(raw: u64) -> bool {
        raw != 0 && unsafe { IsWindow(Some(HWND(raw as usize as *mut _))).as_bool() }
    }

    pub struct SharedMemoryLock {
        handle: HANDLE,
        view: MEMORY_MAPPED_VIEW_ADDRESS,
        event: HANDLE,
    }
    impl SharedMemoryLock {
        pub fn open() -> Result<Self> {
            let mapping = HSTRING::from(LOCK_MAPPING_NAME);
            let handle = unsafe {
                CreateFileMappingW(
                    INVALID_HANDLE_VALUE,
                    None,
                    PAGE_READWRITE,
                    0,
                    LOCK_DATA_SIZE as u32,
                    &mapping,
                )
            }
            .map_err(|error| Error::SharedMemoryUnavailable {
                name: LOCK_MAPPING_NAME.into(),
                source: error.to_string(),
            })?;
            let view = unsafe { MapViewOfFile(handle, FILE_MAP_ALL_ACCESS, 0, 0, LOCK_DATA_SIZE) };
            if view.Value.is_null() {
                let _ = unsafe { CloseHandle(handle) };
                return Err(Error::MapViewFailed {
                    name: LOCK_MAPPING_NAME.into(),
                });
            }
            let event =
                unsafe { CreateEventW(None, false, false, &HSTRING::from(LOCK_EVENT_NAME)) }
                    .map_err(|error| Error::SharedMemoryUnavailable {
                        name: LOCK_EVENT_NAME.into(),
                        source: error.to_string(),
                    })?;
            Ok(Self {
                handle,
                view,
                event,
            })
        }
        pub fn try_lock(&self) -> Option<SharedMemoryGuard<'_>> {
            for _ in 0..MAX_LOCK_SPINS {
                if self
                    .busy()
                    .compare_exchange(0, 1, Ordering::Acquire, Ordering::Relaxed)
                    .is_ok()
                {
                    return Some(SharedMemoryGuard { lock: self });
                }
                std::hint::spin_loop();
            }
            None
        }
        fn waiters(&self) -> &AtomicI32 {
            unsafe { &*self.view.Value.cast::<AtomicI32>() }
        }
        fn busy(&self) -> &AtomicI32 {
            unsafe { &*self.view.Value.cast::<u8>().add(4).cast::<AtomicI32>() }
        }
    }
    impl Drop for SharedMemoryLock {
        fn drop(&mut self) {
            let _ = unsafe { CloseHandle(self.event) };
            let _ = unsafe { UnmapViewOfFile(self.view) };
            let _ = unsafe { CloseHandle(self.handle) };
        }
    }
    unsafe impl Send for SharedMemoryLock {}
    unsafe impl Sync for SharedMemoryLock {}
    pub struct SharedMemoryGuard<'a> {
        lock: &'a SharedMemoryLock,
    }
    impl Drop for SharedMemoryGuard<'_> {
        fn drop(&mut self) {
            self.lock.busy().store(0, Ordering::Release);
            if self.lock.waiters().load(Ordering::Acquire) > 0 {
                let _ = unsafe { SetEvent(self.lock.event) };
            }
        }
    }
}

#[cfg(not(windows))]
mod platform {
    use super::*;
    pub struct SharedMemory;
    pub struct SharedMemoryLock;
    pub struct SharedMemoryGuard;
    pub fn source_window_alive(_: u64) -> bool {
        false
    }
    impl SharedMemory {
        pub fn open(_: &str, _: usize) -> Result<Self> {
            Err(Error::UnsupportedPlatform)
        }
        pub fn read<T: Copy>(&self, _: usize) -> Result<T> {
            Err(Error::UnsupportedPlatform)
        }
        pub fn len(&self) -> usize {
            0
        }
        pub fn is_empty(&self) -> bool {
            true
        }
    }
    impl SharedMemoryLock {
        pub fn open() -> Result<Self> {
            Err(Error::UnsupportedPlatform)
        }
        pub fn try_lock(&self) -> Option<SharedMemoryGuard> {
            None
        }
    }
}

pub(crate) use platform::source_window_alive;
pub use platform::{SharedMemory, SharedMemoryGuard, SharedMemoryLock};
