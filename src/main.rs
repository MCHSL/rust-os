#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(blog_os::test_runner)]
#![reexport_test_harness_main = "test_main"]

extern crate alloc;

use alloc::vec;
use blog_os::networking::add_interface;
use blog_os::networking::socket::SOCKETS;
use blog_os::task::executor::spawn;
use blog_os::task::network::pump_interfaces;
use blog_os::task::{executor::Executor, keyboard, shell::shell, Task};
use blog_os::time::sleep;
use blog_os::{pci, println};
use bootloader::{entry_point, BootInfo};
use core::panic::PanicInfo;
use core::time::Duration;
use smoltcp::iface::SocketSet;
use spin::Mutex;

entry_point!(kernel_main);

fn kernel_main(boot_info: &'static BootInfo) -> ! {
    println!("Starting up");

    blog_os::init(boot_info); // new

    pci::scan_devices();

    let rtl = pci::get_device(0x10EC, 0x8139).unwrap();
    rtl.enable_mastering();
    add_interface(rtl).unwrap();

    let ide = pci::get_device(0x8086, 0x7010).unwrap();
    println!("Prog IF: {:b}", ide.prog);

    SOCKETS.init_once(|| Mutex::new(SocketSet::new(vec![])));

    #[cfg(test)]
    test_main();

    //ata

    let mut executor = Executor::new();
    executor.spawn(Task::new(keyboard::forward_keys()));
    executor.spawn(Task::new(shell()));
    executor.spawn(Task::new(pump_interfaces()));
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
