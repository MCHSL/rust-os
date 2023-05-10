#![no_std]
#![cfg_attr(test, no_main)]
#![feature(custom_test_frameworks)]
#![test_runner(crate::test_runner)]
#![reexport_test_harness_main = "test_main"]
#![feature(abi_x86_interrupt)]
#![allow(clippy::missing_safety_doc)]
#![allow(clippy::new_without_default)]

extern crate alloc;

pub mod acpi;
pub mod allocator;
pub mod drivers;
pub mod gdt;
pub mod interrupts;
pub mod memory;
pub mod pci;
pub mod serial;
pub mod task;
pub mod time;
pub mod vga_buffer;

use core::panic::PanicInfo;

#[cfg(test)]
use bootloader::entry_point;
use bootloader::BootInfo;
use memory::{BootInfoFrameAllocator, FRAME_ALLOCATOR, MAPPER};
use spin::Mutex;
use task::keyboard;
use x86_64::VirtAddr;

#[cfg(test)]
entry_point!(test_kernel_main);

pub trait Testable {
    fn run(&self);
}

impl<T> Testable for T
where
    T: Fn(),
{
    fn run(&self) {
        serial_print!("{}...\t", core::any::type_name::<T>());
        self();
        serial_println!("[ok]");
    }
}

pub fn test_runner(tests: &[&dyn Testable]) {
    serial_println!("Running {} tests", tests.len());
    for test in tests {
        test.run();
    }
    exit_qemu(QemuExitCode::Success);
}

pub fn test_panic_handler(info: &PanicInfo) -> ! {
    serial_println!("[failed]\n");
    serial_println!("Error: {}\n", info);
    exit_qemu(QemuExitCode::Failed);
    hlt_loop();
}

/// Entry point for `cargo test`
#[cfg(test)]
fn test_kernel_main(boot_info: &'static BootInfo) -> ! {
    init(boot_info);
    test_main();
    hlt_loop();
}

#[cfg(test)]
#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    test_panic_handler(info)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum QemuExitCode {
    Success = 0x10,
    Failed = 0x11,
}

pub fn exit_qemu(exit_code: QemuExitCode) {
    use x86_64::instructions::port::Port;

    unsafe {
        let mut port = Port::new(0xf4);
        port.write(exit_code as u32);
    }
}

pub fn init(boot_info: &'static BootInfo) {
    gdt::init();
    interrupts::init_idt();
    time::set_pit_frequency_divider(time::PIT_DIVIDER as u16, 0);
    unsafe {
        let mut pics = interrupts::PICS.lock();
        pics.initialize();
        pics.write_masks(0, 0)
    };

    let phys_mem_offset = VirtAddr::new(boot_info.physical_memory_offset);
    let mut mapper = unsafe { memory::init(phys_mem_offset) };
    let mut frame_allocator = unsafe { BootInfoFrameAllocator::init(&boot_info.memory_map) };

    allocator::init_heap(&mut mapper, &mut frame_allocator).expect("heap initialization failed");

    MAPPER.init_once(|| mapper);
    FRAME_ALLOCATOR.init_once(|| Mutex::new(frame_allocator));

    //read_acpi();

    keyboard::initialize_streams();

    x86_64::instructions::interrupts::enable();
}

pub fn hlt_loop() -> ! {
    loop {
        x86_64::instructions::hlt();
    }
}
