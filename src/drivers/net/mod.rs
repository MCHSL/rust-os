use alloc::vec;
use alloc::{boxed::Box, sync::Arc, vec::Vec};
use smoltcp::iface::SocketHandle;
use smoltcp::socket::{AnySocket, Socket};
use smoltcp::{
    iface::{Config, Interface, SocketSet},
    phy::{self, DeviceCapabilities},
    time::Instant,
    wire::{HardwareAddress, IpAddress, IpCidr, Ipv4Address},
};
use spin::Mutex;

use crate::println;
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

static NET_IFACES: Mutex<Vec<Arc<Mutex<NetworkInterfaceInner>>>> = Mutex::new(Vec::new());

pub fn add_interface<'a>(device: PciDevice) -> Option<NetworkInterface<'static>> {
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
            sockets: SocketSet::new(vec![]),
        }));

        net_ifaces.push(iface_inner.clone());
        Some(NetworkInterface::from(iface_inner))
    } else {
        None
    }
}

pub fn get_interface<'a>(index: usize) -> Option<NetworkInterface<'static>> {
    NET_IFACES
        .lock()
        .get(index)
        .cloned()
        .map(NetworkInterface::from)
}

struct NetworkInterfaceInner<'a> {
    index: usize,
    interface: Interface,
    device: Box<dyn EthernetDevice>,
    sockets: SocketSet<'a>,
}

pub struct NetworkInterface<'a> {
    inner: Arc<Mutex<NetworkInterfaceInner<'a>>>,
}

impl<'a> NetworkInterface<'a> {
    pub fn poll(&mut self) -> bool {
        let NetworkInterfaceInner {
            interface,
            device,
            sockets,
            index: _,
        } = &mut *self.inner.lock();
        let timestamp = Instant::from_secs(time::time() as i64);
        interface.poll(timestamp, &mut **device, sockets)
    }

    pub fn capabilities(&self) -> DeviceCapabilities {
        self.inner.lock().device.get_capabilities()
    }

    pub fn add_socket<S: AnySocket<'a>>(&mut self, socket: S) -> SocketHandle {
        self.inner.lock().sockets.add(socket)
    }

    pub fn with_socket<T: AnySocket<'a>>(
        &mut self,
        handle: SocketHandle,
        mut f: impl FnMut(&mut T),
    ) {
        let mut inner = self.inner.lock();
        let s = inner.sockets.get_mut::<T>(handle);
        f(s);
    }
}

impl<'a> From<Arc<Mutex<NetworkInterfaceInner<'a>>>> for NetworkInterface<'a> {
    fn from(value: Arc<Mutex<NetworkInterfaceInner<'a>>>) -> Self {
        Self { inner: value }
    }
}
