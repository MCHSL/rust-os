use alloc::vec;
use alloc::vec::Vec;
use smoltcp::iface::SocketHandle;
use smoltcp::phy::DeviceCapabilities;
use smoltcp::socket::icmp::*;
use smoltcp::wire::{Icmpv4Packet, Icmpv4Repr, IpAddress};

use crate::networking::wait_for_socket_state_change;
use crate::task::network::notify_tx;

use super::SOCKETS;

pub struct IcmpSocket {
    handle: SocketHandle,
}

impl IcmpSocket {
    pub fn new() -> Self {
        let rx_buffer = PacketBuffer::new(vec![PacketMetadata::EMPTY], vec![0; 256]);
        let tx_buffer = PacketBuffer::new(vec![PacketMetadata::EMPTY], vec![0; 256]);
        let inner = Socket::new(rx_buffer, tx_buffer);
        let handle = SOCKETS.get().unwrap().lock().add(inner);
        Self { handle }
    }

    pub fn with_inner<R>(&mut self, f: impl FnOnce(&mut Socket) -> R) -> R {
        let mut sockets = SOCKETS.get().unwrap().lock();
        let socket = sockets.get_mut(self.handle);
        f(socket)
    }

    fn try_recv(&mut self) -> Option<Result<(Vec<u8>, IpAddress), RecvError>> {
        self.with_inner(|s| {
            if s.can_recv() {
                Some(s.recv().map(|(data, addr)| (data.to_vec(), addr)))
            } else {
                None
            }
        })
    }

    pub async fn recv(&mut self) -> Result<(Vec<u8>, IpAddress), RecvError> {
        loop {
            let res = { self.try_recv() };
            if let Some(res) = res {
                return res;
            }
            wait_for_socket_state_change().await;
        }
    }

    pub fn send(&mut self, to: IpAddress, data: Icmpv4Repr<'_>) {
        self.with_inner(|s| {
            let buffer = s.send(data.buffer_len(), to).unwrap();
            let mut icmp_packet = Icmpv4Packet::new_unchecked(buffer);
            data.emit(&mut icmp_packet, &DeviceCapabilities::default().checksum);
        });
        notify_tx();
    }

    pub fn bind<T: Into<Endpoint>>(&mut self, endpoint: T) -> Result<(), BindError> {
        self.with_inner(|s| s.bind(endpoint))
    }
}

impl Drop for IcmpSocket {
    fn drop(&mut self) {
        SOCKETS.get().unwrap().lock().remove(self.handle);
    }
}
