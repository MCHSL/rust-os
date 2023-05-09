#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(blog_os::test_runner)]
#![reexport_test_harness_main = "test_main"]

extern crate alloc;

use alloc::collections::BTreeMap;
use alloc::vec;
use blog_os::acpi::read_acpi;
use blog_os::memory::{FRAME_ALLOCATOR, MAPPER};
use blog_os::time::{time, time_ms};
use blog_os::{
    allocator,
    memory::{self, BootInfoFrameAllocator},
    task::{executor::Executor, keyboard, shell::shell, Task},
};
use blog_os::{drivers, pci, println, rtl8139, time};
use bootloader::{entry_point, BootInfo};
use byteorder::ByteOrder;
use byteorder::NetworkEndian;
use conquer_once::spin::{Once, OnceCell};
use core::panic::PanicInfo;
use smoltcp::iface::SocketSet;
use smoltcp::socket::icmp;
use smoltcp::time::Instant;
use smoltcp::wire::{Icmpv4Packet, Icmpv4Repr, IpAddress};

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

    let icmp_rx_buffer = icmp::PacketBuffer::new(vec![icmp::PacketMetadata::EMPTY], vec![0; 256]);
    let icmp_tx_buffer = icmp::PacketBuffer::new(vec![icmp::PacketMetadata::EMPTY], vec![0; 256]);
    let icmp_socket = icmp::Socket::new(icmp_rx_buffer, icmp_tx_buffer);
    let icmp_handle = interface.add_socket(icmp_socket);

    let mut send_at = Instant::from_millis(0);
    let mut seq_no = 0;
    let mut received = 0;
    let mut echo_payload = [0xffu8; 40];
    let mut waiting_queue = BTreeMap::new();
    let ident = 0x22b;

    let remote_addr = IpAddress::v4(10, 0, 2, 2);
    let mut send_at = time_ms();
    let interval = 1000;
    let timeout = 1000;
    let count = 4;

    loop {
        interface.poll();

        let timestamp = time_ms();

        let caps = interface.capabilities().checksum;

        interface.with_socket(icmp_handle, |socket: &mut icmp::Socket| {
            if !socket.is_open() {
                socket.bind(icmp::Endpoint::Ident(ident)).unwrap();
            }

            if socket.can_send() && time_ms() >= send_at {
                NetworkEndian::write_i64(&mut echo_payload, time_ms());
                let (icmp_repr, mut icmp_packet) = send_icmp_ping!(
                    Icmpv4Repr,
                    Icmpv4Packet,
                    ident,
                    seq_no,
                    echo_payload,
                    socket,
                    remote_addr
                );
                icmp_repr.emit(&mut icmp_packet, &caps);

                waiting_queue.insert(seq_no, timestamp);
                seq_no += 1;
                send_at += interval;
            }

            if socket.can_recv() {
                let (payload, _) = socket.recv().unwrap();

                match remote_addr {
                    IpAddress::Ipv4(_) => {
                        let icmp_packet = Icmpv4Packet::new_checked(&payload).unwrap();
                        let icmp_repr = Icmpv4Repr::parse(&icmp_packet, &caps).unwrap();
                        get_icmp_pong!(
                            Icmpv4Repr,
                            icmp_repr,
                            payload,
                            waiting_queue,
                            remote_addr,
                            timestamp,
                            received
                        );
                    }
                }
            }

            waiting_queue.retain(|seq, from| {
                if timestamp - *from < timeout {
                    true
                } else {
                    println!("From {remote_addr} icmp_seq={seq} timeout");
                    false
                }
            });
        });
        if seq_no == count as u16 && waiting_queue.is_empty() {
            break;
        }
    }

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
