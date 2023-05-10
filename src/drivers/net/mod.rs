use core::pin::Pin;
use core::sync::atomic::{AtomicBool, Ordering};
use core::task::{self, Poll};
use core::time::Duration;

use alloc::vec;
use alloc::{boxed::Box, sync::Arc, vec::Vec};
use conquer_once::spin::OnceCell;
use futures_util::task::AtomicWaker;
use futures_util::Future;
use smoltcp::iface::SocketHandle;
use smoltcp::socket::icmp::{BindError, Endpoint, RecvError};
use smoltcp::socket::{icmp, AnySocket, Socket};
use smoltcp::wire::{Icmpv4Packet, Icmpv4Repr};
use smoltcp::{
    iface::{Config, Interface, SocketSet},
    phy::{self, DeviceCapabilities},
    time::Instant,
    wire::{HardwareAddress, IpAddress, IpCidr, Ipv4Address},
};
use spin::Mutex;

use crate::println;
use crate::task::network::{notify_tx, RECEIVING_SOCKETS};
use crate::time::{sleep, yield_now};
use crate::{pci::PciDevice, time};

use self::rtl8139::Rtl8139;

pub mod rtl8139;

pub trait EthernetDevice: Send + 'static {
    fn get_capabilities(&self) -> DeviceCapabilities;
    fn mac(&self) -> HardwareAddress;
    fn transmit_packet(&mut self, len: usize);
    fn receive_packet(&mut self) -> Option<Vec<u8>>;
    fn get_transmit_buffer(&mut self, len: usize) -> &mut [u8];
}

pub struct EtherRxToken {
    buffer: Vec<u8>,
}

impl phy::RxToken for EtherRxToken {
    fn consume<R, F>(mut self, f: F) -> R
    where
        F: FnOnce(&mut [u8]) -> R,
    {
        f(&mut self.buffer)
    }
}

pub struct EtherTxToken<'a> {
    device: &'a mut dyn EthernetDevice,
}

impl<'a> phy::TxToken for EtherTxToken<'a> {
    fn consume<R, F>(self, len: usize, f: F) -> R
    where
        F: FnOnce(&mut [u8]) -> R,
    {
        let buf = self.device.get_transmit_buffer(len);
        let result = f(buf);
        self.device.transmit_packet(len);
        result
    }
}

impl phy::Device for dyn EthernetDevice {
    type RxToken<'a> = EtherRxToken;
    type TxToken<'a> = EtherTxToken<'a>;

    fn capabilities(&self) -> DeviceCapabilities {
        self.get_capabilities()
    }

    fn receive(
        &mut self,
        _timestamp: smoltcp::time::Instant,
    ) -> Option<(Self::RxToken<'_>, Self::TxToken<'_>)> {
        self.receive_packet()
            .map(|buffer| (EtherRxToken { buffer }, EtherTxToken { device: self }))
    }

    fn transmit(&mut self, _timestamp: smoltcp::time::Instant) -> Option<Self::TxToken<'_>> {
        Some(EtherTxToken { device: self })
    }
}

pub static NET_IFACES: Mutex<Vec<Arc<Mutex<NetworkInterfaceInner>>>> = Mutex::new(Vec::new());

pub fn add_interface(device: PciDevice) -> Option<NetworkInterface> {
    if device.vendor == 0x10EC && device.id == 0x8139 {
        let mut device: Box<dyn EthernetDevice> = Box::new(Rtl8139::new(device.io_base()));
        let mut config = Config::new();
        config.hardware_addr = Some(device.mac());
        let mut iface = Interface::new(config, &mut *device);

        iface.update_ip_addrs(|ip_addrs| {
            ip_addrs
                .push(IpCidr::new(IpAddress::v4(10, 0, 2, 15), 24))
                .unwrap();
        });
        iface
            .routes_mut()
            .add_default_ipv4_route(Ipv4Address::new(10, 0, 2, 2))
            .unwrap();

        let mut net_ifaces = NET_IFACES.lock();
        let index = net_ifaces.len();

        let iface_inner = Arc::new(Mutex::new(NetworkInterfaceInner {
            index,
            interface: iface,
            device,
        }));

        net_ifaces.push(iface_inner.clone());
        Some(NetworkInterface::from(iface_inner))
    } else {
        None
    }
}

pub fn get_interface(index: usize) -> Option<NetworkInterface> {
    NET_IFACES
        .lock()
        .get(index)
        .cloned()
        .map(NetworkInterface::from)
}

pub fn get_interfaces() -> Vec<NetworkInterface> {
    NET_IFACES
        .lock()
        .iter()
        .cloned()
        .map(NetworkInterface::from)
        .collect()
}

pub struct NetworkInterfaceInner {
    index: usize,
    interface: Interface,
    device: Box<dyn EthernetDevice>,
}

#[derive(Clone)]
pub struct NetworkInterface {
    inner: Arc<Mutex<NetworkInterfaceInner>>,
}

pub static SOCKETS: OnceCell<Mutex<SocketSet>> = OnceCell::uninit();

impl NetworkInterface {
    pub fn poll(&mut self /*, sockets: &mut SocketSet<'a>*/) -> bool {
        //println!("ipoll");
        let NetworkInterfaceInner {
            interface,
            device,
            index: _,
        } = &mut *self.inner.lock();
        let timestamp = Instant::from_secs(time::time() as i64);
        let res = interface.poll(timestamp, &mut **device, &mut SOCKETS.get().unwrap().lock());
        res
    }

    pub fn capabilities(&self) -> DeviceCapabilities {
        self.inner.lock().device.get_capabilities()
    }
}

impl From<Arc<Mutex<NetworkInterfaceInner>>> for NetworkInterface {
    fn from(value: Arc<Mutex<NetworkInterfaceInner>>) -> Self {
        Self { inner: value }
    }
}

pub struct SocketRecvWaiter {
    inner: Arc<SocketRecvWaiterInner>,
}

pub struct SocketRecvWaiterInner {
    pub ready: AtomicBool,
    pub waker: AtomicWaker,
}

impl Future for SocketRecvWaiter {
    type Output = ();
    fn poll(self: Pin<&mut Self>, cx: &mut task::Context<'_>) -> task::Poll<Self::Output> {
        let ready = self.inner.ready.load(Ordering::Relaxed);
        if ready {
            return Poll::Ready(());
        }

        self.inner.waker.register(cx.waker());
        Poll::Pending
    }
}
pub struct IcmpSocket {
    handle: SocketHandle,
}

pub fn wait_for_recv() -> SocketRecvWaiter {
    let waiter = Arc::new(SocketRecvWaiterInner {
        ready: AtomicBool::new(false),
        waker: AtomicWaker::new(),
    });
    RECEIVING_SOCKETS.lock().push(waiter.clone());
    SocketRecvWaiter { inner: waiter }
}

impl IcmpSocket {
    pub fn new() -> Self {
        let rx_buffer = icmp::PacketBuffer::new(vec![icmp::PacketMetadata::EMPTY], vec![0; 256]);
        let tx_buffer = icmp::PacketBuffer::new(vec![icmp::PacketMetadata::EMPTY], vec![0; 256]);
        let inner = icmp::Socket::new(rx_buffer, tx_buffer);
        let handle = SOCKETS.get().unwrap().lock().add(inner);
        Self { handle }
    }

    pub fn with_inner<R>(&mut self, f: impl FnOnce(&mut icmp::Socket) -> R) -> R {
        let mut sockets = SOCKETS.get().unwrap().lock();
        let socket = sockets.get_mut(self.handle);
        let res = f(socket);
        res
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
            wait_for_recv().await;
        }
    }

    pub async fn send(&mut self, to: IpAddress, data: Icmpv4Repr<'_>) {
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
