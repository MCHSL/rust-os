#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(blog_os::test_runner)]
#![reexport_test_harness_main = "test_main"]

extern crate alloc;

use alloc::collections::BTreeMap;
use alloc::vec;
use alloc::vec::Vec;
use blog_os::acpi::read_acpi;
use blog_os::drivers::net::{IcmpSocket, SOCKETS};
use blog_os::memory::{FRAME_ALLOCATOR, MAPPER};
use blog_os::task::network::{keep_pumping_interfaces, pump_interfaces};
use blog_os::time::{sleep, time, time_ms};
use blog_os::{
    allocator,
    memory::{self, BootInfoFrameAllocator},
    task::{executor::Executor, keyboard, shell::shell, Task},
};
use blog_os::{drivers, pci, println, time};
use bootloader::{entry_point, BootInfo};
use byteorder::ByteOrder;
use byteorder::NetworkEndian;
use conquer_once::spin::{Once, OnceCell};
use core::panic::PanicInfo;
use core::time::Duration;
use smoltcp::iface::SocketSet;
use smoltcp::socket::icmp;
use smoltcp::time::Instant;
use smoltcp::wire::{Icmpv4Packet, Icmpv4Repr, IpAddress};
use spin::Mutex;

use x86_64::structures::paging::OffsetPageTable;
use x86_64::VirtAddr;

entry_point!(kernel_main);

macro_rules! send_icmp_ping {
    ( $repr_type:ident, $packet_type:ident, $ident:expr, $seq_no:expr,
      $echo_payload:expr, $socket:expr, $remote_addr:expr ) => {{
        let icmp_repr = $repr_type::EchoRequest {
            ident: $ident,
            seq_no: $seq_no,
            data: &$echo_payload,
        };

        let icmp_payload = $socket.send(icmp_repr.buffer_len(), $remote_addr).unwrap();

        let icmp_packet = $packet_type::new_unchecked(icmp_payload);
        (icmp_repr, icmp_packet)
    }};
}

macro_rules! get_icmp_pong {
    ( $repr_type:ident, $repr:expr, $payload:expr, $waiting_queue:expr, $remote_addr:expr,
      $timestamp:expr, $received:expr ) => {{
        if let $repr_type::EchoReply { seq_no, data, .. } = $repr {
            if let Some(_) = $waiting_queue.get(&seq_no) {
                let packet_timestamp_ms = NetworkEndian::read_i64(data);
                println!(
                    "{} bytes from {}: icmp_seq={}, time={}ms",
                    data.len(),
                    $remote_addr,
                    seq_no,
                    $timestamp - packet_timestamp_ms
                );
                $waiting_queue.remove(&seq_no);
                $received += 1;
            }
        }
    }};
}

fn kernel_main(boot_info: &'static BootInfo) -> ! {
    println!("Starting up");

    blog_os::init(boot_info); // new

    pci::scan_devices();

    let rtl = pci::get_device(0x10EC, 0x8139).unwrap();
    rtl.enable_mastering();
    let mut interface = drivers::net::add_interface(rtl).unwrap();

    SOCKETS.init_once(|| Mutex::new(SocketSet::new(vec![])));

    #[cfg(test)]
    test_main();

    let mut executor = Executor::new();
    let spawner = executor.spawner();
    executor.spawn(Task::new(keyboard::forward_keys()));
    //executor.spawn(Task::new(keyboard::print_keys()));
    executor.spawn(Task::new(shell(spawner)));
    executor.spawn(Task::new(keep_pumping_interfaces()));
    //executor.spawn(Task::new(executor_heartbeat()));
    executor.run();

    //println!("Done!");
    //hlt_loop();
}

async fn executor_heartbeat() {
    loop {
        println!("Executor ok!");
        sleep(Duration::from_millis(1000)).await;
    }
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
