#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(blog_os::test_runner)]
#![reexport_test_harness_main = "test_main"]

extern crate alloc;

use blog_os::acpi::read_acpi;
use blog_os::memory::{FRAME_ALLOCATOR, MAPPER};
use blog_os::{
    allocator,
    memory::{self, BootInfoFrameAllocator},
    task::{executor::Executor, keyboard, shell::shell, Task},
};
use blog_os::{pci, println, rtl8139};
use bootloader::{entry_point, BootInfo};
use conquer_once::spin::{Once, OnceCell};
use core::panic::PanicInfo;
use x86_64::structures::paging::OffsetPageTable;
use x86_64::VirtAddr;

entry_point!(kernel_main);

fn kernel_main(boot_info: &'static BootInfo) -> ! {
    println!("Starting up");

    blog_os::init(boot_info); // new

    pci::scan_devices();
    rtl8139::init();

    #[cfg(test)]
    test_main();

    let mut executor = Executor::new();
    let spawner = executor.spawner();
    executor.spawn(Task::new(keyboard::forward_keys()));
    //executor.spawn(Task::new(keyboard::print_keys()));
    executor.spawn(Task::new(shell(spawner)));
    executor.run();

    //println!("Done!");
    //hlt_loop();
}

/// This function is called on panic.
#[cfg(not(test))]
#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    println!("{}", info);
    loop {}
}

#[cfg(test)]
#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    blog_os::test_panic_handler(info)
}
