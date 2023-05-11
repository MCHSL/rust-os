use alloc::vec;
use smoltcp::{
    iface::SocketHandle,
    socket::tcp::{self, Socket},
    wire::IpAddress,
};

use crate::{
    networking::{wait_for_socket_state_change, NetworkInterface},
    println,
    task::network::notify_tx,
};

use super::SOCKETS;

pub struct TcpSocket {
    handle: SocketHandle,
}

impl TcpSocket {
    pub fn new() -> Self {
        let rx_buffer = tcp::SocketBuffer::new(vec![0; 1024]);
        let tx_buffer = tcp::SocketBuffer::new(vec![0; 1024]);
        let inner = tcp::Socket::new(rx_buffer, tx_buffer);
        let handle = SOCKETS.get().unwrap().lock().add(inner);
        Self { handle }
    }

    pub fn with_inner<R>(&mut self, f: impl FnOnce(&mut Socket) -> R) -> R {
        let mut sockets = SOCKETS.get().unwrap().lock();
        let socket = sockets.get_mut(self.handle);
        f(socket)
    }

    pub async fn connect(
        &mut self,
        iface: &mut NetworkInterface,
        address: IpAddress,
        port: u16,
    ) -> Result<(), tcp::ConnectError> {
        let result = self.with_inner(|s| {
            iface.with_inner(|i| s.connect(i.interface.context(), (address, port), 1111))
        });
        notify_tx();
        wait_for_socket_state_change().await;
        result
    }

    pub async fn send(&mut self, data: &[u8]) -> Result<usize, tcp::SendError> {
        loop {
            let res = self.with_inner(|s| {
                if !s.may_send() {
                    None
                } else {
                    Some(s.send_slice(data))
                }
            });

            if let Some(res) = res {
                notify_tx();
                return res;
            }

            wait_for_socket_state_change().await;
        }
    }

    pub async fn recv(&mut self, data: &mut [u8]) -> Result<usize, tcp::RecvError> {
        loop {
            let res = self.with_inner(|s| {
                if !s.can_recv() {
                    None
                } else {
                    Some(s.recv_slice(data))
                }
            });

            if let Some(res) = res {
                return res;
            }

            wait_for_socket_state_change().await;
        }
    }
}
