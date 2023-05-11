use alloc::{boxed::Box, sync::Arc, vec::Vec};
use smoltcp::{
    iface::{Config, Interface},
    phy::{self, DeviceCapabilities},
    time::Instant,
    wire::{HardwareAddress, IpAddress, IpCidr, Ipv4Address},
};
use spin::Mutex;

use crate::drivers::net::rtl8139::Rtl8139;
use crate::task::network::{NotificationWaiter, NotificationWaiterInner, RECEIVING_SOCKETS};
use crate::{pci::PciDevice, time};

use self::socket::SOCKETS;

pub mod socket;

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
    pub index: usize,
    interface: Interface,
    device: Box<dyn EthernetDevice>,
}

#[derive(Clone)]
pub struct NetworkInterface {
    inner: Arc<Mutex<NetworkInterfaceInner>>,
}

impl NetworkInterface {
    pub fn poll(&mut self) -> bool {
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

pub fn wait_for_socket_rx() -> NotificationWaiter {
    let waiter = Arc::new(NotificationWaiterInner::new());
    RECEIVING_SOCKETS.lock().push(waiter.clone());
    NotificationWaiter { inner: waiter }
}
