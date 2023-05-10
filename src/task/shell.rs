use core::time::Duration;

use alloc::{collections::BTreeMap, string::String, vec::Vec};
use byteorder::{ByteOrder, NetworkEndian};
use futures_util::{future::select, StreamExt};
use pc_keyboard::DecodedKey;
use smoltcp::{
    socket::icmp,
    time::Instant,
    wire::{Icmpv4Packet, Icmpv4Repr, IpAddress},
};
use x86_64::instructions::port::Port;

use crate::{
    backspace,
    drivers::net::{get_interface, rtl8139::rtl_receive, IcmpSocket},
    print, println,
    time::{sleep, time_ms, yield_now},
};

use super::{executor::TaskSpawner, keyboard::KeyStream, network::pump_interfaces};

pub async fn shell(_spawner: TaskSpawner) {
    let mut stream = KeyStream::new();
    let mut buffer = String::new();

    loop {
        print!("# ");
        loop {
            if let Some(key) = { stream.next().await } {
                match key {
                    DecodedKey::Unicode(key) => {
                        if key == '\n' {
                            break;
                        } else if key == char::from(8) {
                            if !buffer.is_empty() {
                                backspace!();
                                buffer.pop();
                            }
                        } else {
                            buffer.push(key);
                            print!("{key}");
                        }
                    }
                    DecodedKey::RawKey(_key) => {
                        //print!("{key:?}");
                    }
                }
            }
        }
        println!("");
        let mut input = buffer.split_ascii_whitespace();
        if let Some(command) = input.next() {
            match command {
                "hello" => {
                    println!("world!");
                }
                "echo" => {
                    let rest = input.collect::<Vec<&str>>().join(" ");
                    println!("{rest}");
                }
                "sleep" => {
                    match input.next() {
                        Some(dur) => match dur.parse() {
                            Ok(dur) => sleep(Duration::from_millis(dur)).await,
                            Err(e) => println!("Error parsing argument: {e}"),
                        },
                        None => println!("Missing argument"),
                    };
                }
                "ping" => {
                    match input.next() {
                        Some(addr) => match addr.parse() {
                            Ok(addr) => ping(addr).await,
                            Err(_) => println!("Invalid address"),
                        },
                        None => println!("Missing argument"),
                    };
                }
                _ => {
                    println!("Unrecognized commmand: {}", command)
                }
            }
        }

        buffer.clear();
    }
}

async fn ping(remote_addr: IpAddress) {
    let interface = get_interface(0).unwrap();
    let mut icmp_socket = IcmpSocket::new();

    let mut echo_payload = [0xffu8; 40];
    let ident = 0x22b;
    let count = 4;

    icmp_socket.bind(icmp::Endpoint::Ident(ident)).unwrap();

    for seq_no in 0..count {
        NetworkEndian::write_i64(&mut echo_payload, time_ms());
        let icmp_repr = Icmpv4Repr::EchoRequest {
            ident,
            seq_no,
            data: &echo_payload,
        };

        icmp_socket.send(remote_addr, icmp_repr).await;
        let (data, _addr) = icmp_socket.recv().await.unwrap();
        let icmp_packet = Icmpv4Packet::new_checked(&data).unwrap();
        let icmp_repr =
            Icmpv4Repr::parse(&icmp_packet, &interface.capabilities().checksum).unwrap();
        if let Icmpv4Repr::EchoReply { seq_no, data, .. } = icmp_repr {
            let packet_timestamp_ms = NetworkEndian::read_i64(data);
            println!(
                "{} bytes from {}: icmp_seq={}, time={}ms",
                data.len(),
                remote_addr,
                seq_no,
                time_ms() - packet_timestamp_ms
            );
        }

        if seq_no != count - 1 {
            sleep(Duration::from_millis(1000)).await;
        }
    }
}
