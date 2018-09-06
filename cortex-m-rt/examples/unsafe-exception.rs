#![deny(warnings)]
#![no_main]
#![no_std]

extern crate cortex_m_rt;
extern crate panic_semihosting;

use cortex_m_rt::{entry, exception};

#[entry]
fn foo() -> ! {
    loop {}
}

#[exception]
unsafe fn SysTick() {}
