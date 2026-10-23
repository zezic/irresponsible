//! Optional callback allocation accounting. FFI backend work outside our
//! callbacks is intentionally outside this measurement.
use std::{
    alloc::{GlobalAlloc, Layout, System},
    cell::Cell,
    sync::atomic::{AtomicU64, Ordering},
};

thread_local! { static IN_CALLBACK: Cell<bool> = const { Cell::new(false) }; }
pub static ALLOCATIONS: AtomicU64 = AtomicU64::new(0);
pub static DEALLOCATIONS: AtomicU64 = AtomicU64::new(0);
struct Allocator;
#[global_allocator]
static ALLOCATOR: Allocator = Allocator;

// SAFETY: every operation forwards its original layout/pointer to System.
unsafe impl GlobalAlloc for Allocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if IN_CALLBACK.try_with(Cell::get).unwrap_or(false) {
            ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        }
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        if IN_CALLBACK.try_with(Cell::get).unwrap_or(false) {
            DEALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        }
        unsafe {
            System.dealloc(pointer, layout);
        }
    }
    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        if IN_CALLBACK.try_with(Cell::get).unwrap_or(false) {
            ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        }
        unsafe { System.realloc(pointer, layout, size) }
    }
}
pub struct Callback;
pub fn enter() -> Callback {
    IN_CALLBACK.with(|flag| {
        assert!(!flag.replace(true), "callback audit cannot nest");
    });
    Callback
}
impl Drop for Callback {
    fn drop(&mut self) {
        IN_CALLBACK.with(|flag| flag.set(false));
    }
}
