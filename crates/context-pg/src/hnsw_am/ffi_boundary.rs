//! Callback-local pointer capability created only by guarded FFI wrappers.

use core::ptr::NonNull;
use std::marker::PhantomData;

use pgrx::prelude::*;

use crate::error::raise_sql_error;

/// Callback-local lifetime anchor for PostgreSQL-owned pointers.
///
/// Capabilities created through this scope borrow it, so the compiler prevents
/// them from escaping the guarded callback stack frame that owns the scope.
#[derive(Debug)]
pub(super) struct PgCallbackScope {
    _private: (),
}

impl PgCallbackScope {
    /// Starts a pointer-capability scope inside one guarded PostgreSQL callback.
    ///
    /// # Safety
    ///
    /// The caller must be executing inside a guarded PostgreSQL callback and
    /// must drop this scope before returning to PostgreSQL.
    pub(super) const unsafe fn new() -> Self {
        Self { _private: () }
    }

    /// Creates a callback-local shared capability.
    ///
    /// # Safety
    ///
    /// `pointer` must be non-null and valid for shared access to `T` until this
    /// scope is dropped. PostgreSQL must not mutate the referent while a shared
    /// borrow obtained from the capability is live.
    pub(super) unsafe fn borrow<'callback, T>(
        &'callback self,
        pointer: *mut T,
        parameter: &'static str,
    ) -> PgCallbackRef<'callback, T> {
        let Some(pointer) = NonNull::new(pointer) else {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                format!("HNSW callback received null {parameter}"),
            );
        };
        PgCallbackRef {
            pointer,
            marker: PhantomData,
        }
    }

    /// Creates an optional callback-local shared capability.
    ///
    /// # Safety
    ///
    /// Every non-null pointer must satisfy the same validity and access
    /// requirements as [`Self::borrow`].
    pub(super) unsafe fn borrow_optional<'callback, T>(
        &'callback self,
        pointer: *mut T,
    ) -> Option<PgCallbackRef<'callback, T>> {
        NonNull::new(pointer).map(|pointer| PgCallbackRef {
            pointer,
            marker: PhantomData,
        })
    }

    /// Creates a callback-local exclusive capability.
    ///
    /// # Safety
    ///
    /// `pointer` must be non-null, initialized, valid, and exclusively
    /// accessible for `T` until this scope is dropped.
    pub(super) unsafe fn borrow_mut<'callback, T>(
        &'callback self,
        pointer: *mut T,
        parameter: &'static str,
    ) -> PgCallbackMut<'callback, T> {
        let Some(pointer) = NonNull::new(pointer) else {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                format!("HNSW callback received null {parameter}"),
            );
        };
        PgCallbackMut {
            pointer,
            marker: PhantomData,
        }
    }

    /// Creates a callback-local shared slice capability.
    ///
    /// # Safety
    ///
    /// For nonzero `len`, `pointer` must be non-null, aligned, initialized,
    /// and readable for `len` consecutive `T` values until this scope is
    /// dropped. PostgreSQL must not mutate them while a shared slice is live.
    pub(super) unsafe fn borrow_slice<'callback, T>(
        &'callback self,
        pointer: *mut T,
        len: usize,
        parameter: &'static str,
    ) -> PgCallbackSlice<'callback, T> {
        let pointer = if len == 0 {
            NonNull::dangling()
        } else {
            let Some(pointer) = NonNull::new(pointer) else {
                raise_sql_error(
                    PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                    format!("HNSW callback received null {parameter} for {len} values"),
                );
            };
            pointer
        };
        PgCallbackSlice {
            pointer,
            len,
            marker: PhantomData,
        }
    }
}

/// Non-null shared PostgreSQL callback pointer established by a guarded
/// wrapper.
#[derive(Debug)]
pub(super) struct PgCallbackRef<'callback, T> {
    pointer: NonNull<T>,
    marker: PhantomData<&'callback T>,
}

impl<T> PgCallbackRef<'_, T> {
    pub(super) const fn as_ptr(&self) -> *mut T {
        self.pointer.as_ptr()
    }

    pub(super) fn as_ref(&self) -> &T {
        // SAFETY: The only constructors require a callback-local live pointer;
        // the returned borrow is tied to this capability.
        unsafe { self.pointer.as_ref() }
    }
}

/// Non-null exclusive PostgreSQL callback pointer established by a guarded
/// wrapper.
#[derive(Debug)]
pub(super) struct PgCallbackMut<'callback, T> {
    pointer: NonNull<T>,
    marker: PhantomData<&'callback mut T>,
}

impl<T> PgCallbackMut<'_, T> {
    pub(super) const fn as_ptr(&self) -> *mut T {
        self.pointer.as_ptr()
    }

    pub(super) fn as_ref(&self) -> &T {
        // SAFETY: The constructor requires a live exclusive pointer and the
        // returned shared borrow is tied to this capability.
        unsafe { self.pointer.as_ref() }
    }

    pub(super) fn as_mut(&mut self) -> &mut T {
        // SAFETY: This capability is non-clonable, its constructor requires
        // exclusive access, and the borrow is tied to `&mut self`.
        unsafe { self.pointer.as_mut() }
    }

    pub(super) fn write(&mut self, value: T) {
        *self.as_mut() = value;
    }
}

/// Length-bearing shared array borrowed for one PostgreSQL callback.
#[derive(Debug)]
pub(super) struct PgCallbackSlice<'callback, T> {
    pointer: NonNull<T>,
    len: usize,
    marker: PhantomData<&'callback [T]>,
}

impl<T> PgCallbackSlice<'_, T> {
    pub(super) const fn len(&self) -> usize {
        self.len
    }

    pub(super) fn as_slice(&self) -> &[T] {
        // SAFETY: Construction proves readability for `len` elements, or uses
        // a valid dangling pointer for the zero-length case.
        unsafe { std::slice::from_raw_parts(self.pointer.as_ptr(), self.len) }
    }
}

/// Rust value registered for exactly-once destruction by a PostgreSQL memory
/// context, with optional early value release on a normal callback path.
#[derive(Debug)]
pub(super) struct PgMemoryContextDropSlot<T> {
    value: Option<T>,
}

impl<T> PgMemoryContextDropSlot<T> {
    pub(super) fn new(value: T) -> Self {
        Self { value: Some(value) }
    }

    pub(super) fn value_mut(&mut self) -> Option<&mut T> {
        self.value.as_mut()
    }

    pub(super) fn take(&mut self) -> Option<T> {
        self.value.take()
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::rc::Rc;

    use super::*;

    struct DropProbe(Rc<Cell<usize>>);

    impl Drop for DropProbe {
        fn drop(&mut self) {
            self.0.set(self.0.get() + 1);
        }
    }

    #[test]
    fn callback_capabilities_separate_shared_and_exclusive_access() {
        let mut value = 7_u64;
        // SAFETY: The test scope does not outlive this stack frame.
        let scope = unsafe { PgCallbackScope::new() };
        {
            // SAFETY: `value` is live and shared only for this scope.
            let shared = unsafe { scope.borrow(&mut value, "fixture") };
            assert_eq!(*shared.as_ref(), 7);
        }

        // SAFETY: `value` is live and exclusively accessed through `exclusive`.
        let mut exclusive = unsafe { scope.borrow_mut(&mut value, "fixture") };
        exclusive.write(9);
        assert_eq!(value, 9);
    }

    #[test]
    fn callback_slice_carries_the_validated_extent() {
        let mut values = [3_u32, 5, 8];
        // SAFETY: The test scope does not outlive this stack frame.
        let scope = unsafe { PgCallbackScope::new() };
        // SAFETY: The array is initialized and remains shared for the test.
        let slice = unsafe { scope.borrow_slice(values.as_mut_ptr(), values.len(), "values") };
        assert_eq!(slice.len(), 3);
        assert_eq!(slice.as_slice(), &[3, 5, 8]);

        // SAFETY: A zero-length slice never dereferences the null pointer.
        let empty = unsafe { scope.borrow_slice::<u32>(std::ptr::null_mut(), 0, "empty") };
        assert!(empty.as_slice().is_empty());
    }

    #[test]
    fn memory_context_slot_drops_the_value_exactly_once_on_every_path() {
        let normal_drops = Rc::new(Cell::new(0));
        let mut normal = PgMemoryContextDropSlot::new(DropProbe(Rc::clone(&normal_drops)));
        drop(normal.take());
        assert_eq!(normal_drops.get(), 1);
        assert!(normal.take().is_none());
        drop(normal);
        assert_eq!(normal_drops.get(), 1);

        let error_drops = Rc::new(Cell::new(0));
        let error = PgMemoryContextDropSlot::new(DropProbe(Rc::clone(&error_drops)));
        drop(error);
        assert_eq!(error_drops.get(), 1);
    }
}
