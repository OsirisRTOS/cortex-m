//! Synchronization primitives.

use core::cell::UnsafeCell;
use core::mem::MaybeUninit;

#[cfg(all(feature = "atomic-cas"))]
use crate::atomic::AtomicBool;

use crate::atomic::AtomicU8;
use crate::atomic::Ordering;

use crate::asm;

#[cfg(all(not(feature = "atomic-cas"), not(cortex_m)))]
compile_error!("This target is not supported.");

/// A mutual exclusion primitive, facilitating busy-waiting.
pub struct SpinLock {
    #[cfg(all(feature = "atomic-cas"))]
    lock: AtomicBool,
}

impl SpinLock {
    /// Creates a new SpinLock.
    pub const fn new() -> Self {
        SpinLock {
            #[cfg(all(feature = "atomic-cas"))]
            lock: AtomicBool::new(false),
        }
    }

    /// Waits until the SpinLock can be acquired and locks it.
    /// On a single-core system, this function only disables interrupts.
    pub fn lock(&self) {
        #[cfg(all(feature = "atomic-cas"))]
        {
            while self
                .lock
                .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
                .is_err()
            {
                asm::nop();
            }
            return;
        }
        #[cfg(all(not(feature = "atomic-cas"), cortex_m))]
        {
            use crate::interrupt;
            interrupt::disable();
            return;
        }
    }

    /// Tries to lock the SpinLock.
    /// Returns `true` if the lock was acquired.
    /// On a single-core system, this function only disables interrupts.
    pub fn try_lock(&self) -> bool {
        #[cfg(all(feature = "atomic-cas"))]
        {
            return !self.lock.swap(true, Ordering::Acquire);
        }

        #[cfg(all(not(feature = "atomic-cas"), cortex_m))]
        {
            use crate::interrupt;
            interrupt::disable();
            return true;
        }
    }

    /// Unlocks the SpinLock.
    /// On a single-core system, this function only enables interrupts.
    /// Returns `true` if the lock was released.
    ///
    /// # Safety
    /// Precondition: The SpinLock must be locked by the current thread.
    /// Postcondition: The SpinLock is unlocked.
    pub unsafe fn unlock(&self) {
        #[cfg(all(feature = "atomic-cas"))]
        {
            return self.lock.store(false, Ordering::Release);
        }

        #[cfg(all(not(feature = "atomic-cas"), cortex_m))]
        {
            use crate::interrupt;
            return unsafe { interrupt::enable() };
        }
    }
}

/// A synchronization primitive that can be used to block a thread until a value is ready.
/// The procedure is as follows:
/// 1. The Caller calls step(NOT_READY) to indicate that it is about to start the initialization process.
/// 2. The Caller initializes the value.
/// 3. The Caller calls step(IN_TRANSIT) to indicate that the value is ready.
/// If step 1 fails, the value is already being initialized and the Caller must wait until is() returns true.
pub struct Ready {
    ready: AtomicU8,
}

impl Ready {
    const READY: u8 = 2;
    const IN_TRANSIT: u8 = 1;
    const NOT_READY: u8 = 0;

    /// Initializes a new Ready.
    pub const fn new() -> Self {
        Self {
            ready: AtomicU8::new(0),
        }
    }

    /// Move the Ready to the next state, if it is in state `from`.
    pub fn step(&self, from: u8) -> bool {
        self.forward(from, from + 1)
    }

    /// Move the Ready to state `to` if it is in state `from`.
    fn forward(&self, _from: u8, _to: u8) -> bool {
        return self
            .ready
            .compare_exchange(_from, _to, Ordering::AcqRel, Ordering::Acquire)
            .is_ok();
    }

    /// Returns true if the value is ready.
    pub fn is(&self) -> bool {
        return self.ready.load(Ordering::Acquire) == Self::READY;
    }
}

/// A synchronization primitive that represents a value that is initialized at most once.
pub struct OnceCell<T> {
    value: UnsafeCell<MaybeUninit<T>>,
    init: Ready,
}

/// Safety:
/// 1. A `value` is only written to atomically and once.
/// 2. A `value` is only readable from after the initialization process is finished.
/// 3. A `init` is only written and read from atomically.
unsafe impl<T> Sync for OnceCell<T> {}

impl<T> OnceCell<T> {
    /// Initializes a new OnceCell.
    pub const fn new() -> Self {
        Self {
            value: UnsafeCell::new(MaybeUninit::uninit()),
            init: Ready::new(),
        }
    }

    /// Returns a reference to the value if it is initialized.
    pub fn get(&self) -> Option<&T> {
        if self.init.is() {
            // Safety:
            // 1. By contract, is the value initialized if init.is() returns true.
            // 2. No writes are allowed to the value after the initialization process is finished.
            Some(unsafe { self.get_unchecked() })
        } else {
            None
        }
    }

    /// Sets the value if it is not already initialized, and returns a reference to the value.
    pub fn set_or_get(&self, value: T) -> &T {
        if let Some(value) = self.set(value) {
            value
        } else {
            // If we reach this point, initialization is already in progress.
            while !self.init.is() {
                asm::nop();
            }
            // Safety:
            // 1. By contract, is the value initialized if init.is() returns true.
            // 2. No writes are allowed to the value after the initialization process is finished.
            unsafe { self.get_unchecked() }
        }
    }

    /// Sets the value if it is not already initialized, and returns a reference to the value.
    pub fn do_or_get<F>(&self, f: F) -> &T
    where
        F: FnOnce() -> T,
    {
        self.set_or_get(f())
    }

    /// Sets the value if it is not already initialized, returns a reference to the value if it was not set previously.
    pub fn set(&self, value: T) -> Option<&T> {
        if self.init.is() {
            return None;
        }

        if self.init.step(Ready::NOT_READY) {
            // Safety: We are now in the IN_TRANSIT state, so we are the only ones that can write to the value.
            // We are also the only ones that can read from the value.
            unsafe {
                self.value.get().write(MaybeUninit::new(value));
            }

            if self.init.step(Ready::IN_TRANSIT) {
                // Safety: We are now in the READY state, so no writes can happen to the value.
                // 1. It is safe to create a immutable reference to the value.
                // 2. We initialized the value, so it is safe to return a reference to it.
                return Some(unsafe { self.get_unchecked() });
            }

            // By contract, only the thread that started the initialization process can finish it.
            unreachable!();
        }

        return None;
    }

    /// Returns a reference to the value, unchecked.
    ///
    /// # Safety
    /// Preconditions: The value must be initialized.
    /// Postconditions: The value is returned.
    unsafe fn get_unchecked(&self) -> &T {
        unsafe { (&*self.value.get()).assume_init_ref() }
    }
}
