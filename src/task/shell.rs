use core::time::Duration;

use alloc::{string::String, vec, vec::Vec};
use byteorder::{ByteOrder, NetworkEndian};
use futures_util::StreamExt;
use pc_keyboard::DecodedKey;
use smoltcp::{
    socket::icmp,
    wire::{Icmpv4Packet, Icmpv4Repr, IpAddress},
};

use crate::{
    backspace,
    networking::{
        get_interface,
        socket::{
            icmp::IcmpSocket,
            tcp::{TcpListener, TcpStream},
        },
    },
    print, println,
    task::executor::spawn,
    time::{sleep, time_ms},
};

use super::keyboard::KeyStream;

pub async fn shell() {
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
                "send" => {
                    match input.next() {
                        Some(addr) => match addr.parse() {
                            Ok(addr) => {
                                let text = input.collect::<Vec<&str>>().join(" ");
                                connect(addr, text).await
                            }
                            Err(_) => println!("Invalid address"),
                        },
                        None => println!("Missing argument"),
                    };
                }
                "listen" => {
                    match input.next() {
                        Some(addr) => match addr.parse() {
                            Ok(addr) => listen(addr).await,
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

        icmp_socket.send(remote_addr, icmp_repr);
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

async fn connect(remote_addr: IpAddress, text: String) {
    let mut interface = get_interface(0).unwrap();
    let mut socket = TcpStream::new();

    socket
        .connect(&mut interface, remote_addr, 80)
        .await
        .unwrap();
    socket.send(text.as_bytes()).await.unwrap();
    let mut buffer = vec![0; 1024];
    let read = socket.recv(buffer.as_mut_slice()).await.unwrap();
    let s = String::from_utf8_lossy(&buffer[..read]);
    println!("{s}");
}

async fn listen(port: u16) {
    //let mut interface = get_interface(0).unwrap();
    let mut listener = TcpListener::new();
    println!("Listening on {port}");
    listener.listen(port).unwrap();

    loop {
        let mut stream = listener.accept().await;
        println!("New client!");
        spawn(async move {
            loop {
                let mut buffer = vec![0; 1024];
                let read = stream.recv(buffer.as_mut_slice()).await.unwrap();
                let s = String::from_utf8_lossy(&buffer[..read]);
                println!(">{s}");
            }
        })
    }
}
