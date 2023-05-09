use core::sync::atomic::{AtomicUsize, Ordering};

use alloc::sync::Arc;
use alloc::vec;
use alloc::{format, string::ToString, vec::Vec};
use conquer_once::spin::OnceCell;
use smoltcp::iface::{Interface, SocketSet};
use smoltcp::phy::{self, Device, DeviceCapabilities, Medium};
use smoltcp::socket::icmp;
use smoltcp::time::Instant;
use smoltcp::wire::{
    EthernetAddress, HardwareAddress, Icmpv4Packet, Icmpv4Repr, IpAddress, IpCidr, Ipv4Address,
};
use spin::Mutex;
use x86_64::{
    instructions::{hlt, port::Port},
    VirtAddr,
};

use crate::allocator::PhysBuf;
use crate::time;
use crate::{memory, pci::get_device, print, println};

const RX_BUFFER_IDX: usize = 0;

const MTU: usize = 1536;

const RX_BUFFER_PAD: usize = 16;
const RX_BUFFER_LEN: usize = 8192 << RX_BUFFER_IDX;

const TX_BUFFER_LEN: usize = 2048;
const TX_BUFFERS_COUNT: usize = 4;
const ROK: u16 = 0x01;

const CR_RST: u8 = 1 << 4; // Reset
const CR_RE: u8 = 1 << 3; // Receiver Enable
const CR_TE: u8 = 1 << 2; // Transmitter Enable
const CR_BUFE: u8 = 1 << 0; // Buffer Empty

// Rx Buffer Length
const RCR_RBLEN: u32 = (RX_BUFFER_IDX << 11) as u32;

// When the WRAP bit is set, the nic will keep moving the rest
// of the packet data into the memory immediately after the
// end of the Rx buffer instead of going back to the begining
// of the buffer. So the buffer must have an additionnal 1500 bytes.
const RCR_WRAP: u32 = 1 << 7;

const RCR_AB: u32 = 1 << 3; // Accept Broadcast packets
const RCR_AM: u32 = 1 << 2; // Accept Multicast packets
const RCR_APM: u32 = 1 << 1; // Accept Physical Match packets
const RCR_AAP: u32 = 1 << 0; // Accept All Packets

// Interframe Gap Time
const TCR_IFG: u32 = 3 << 24;

// Max DMA Burst Size per Tx DMA Burst
// 000 = 16 bytes
// 001 = 32 bytes
// 010 = 64 bytes
// 011 = 128 bytes
// 100 = 256 bytes
// 101 = 512 bytes
// 110 = 1024 bytes
// 111 = 2048 bytes
//const TCR_MXDMA0: u32 = 1 << 8;
const TCR_MXDMA1: u32 = 1 << 9;
const TCR_MXDMA2: u32 = 1 << 10;

const TOK: u32 = 1 << 15; // Transmit OK
const OWN: u32 = 1 << 13; // DMA operation completed

pub struct Rtl8139 {
    io_base: u16,
    config1: Port<u8>,
    command: Port<u8>,
    rbstart: Port<u32>,
    imr: Port<u16>,
    tx_config: Port<u32>,
    rx_config: Port<u32>,
    pub capr: Port<u16>,
    pub cba: Port<u16>,

    transmit_start: [Port<u32>; 4],
    transmit_status: [Port<u32>; 4],

    tx_buffers: [PhysBuf; 4],
    tx_id: AtomicUsize,
    rx_buffer: PhysBuf,
    rx_offset: usize,
}

impl Rtl8139 {
    pub fn new(io_base: u16) -> Self {
        Self {
            io_base,
            config1: Port::new(io_base + 0x52),
            command: Port::new(io_base + 0x37),
            rbstart: Port::new(io_base + 0x30),
            imr: Port::new(io_base + 0x3C),
            tx_config: Port::new(io_base + 0x40),
            rx_config: Port::new(io_base + 0x44),
            capr: Port::new(io_base + 0x38),
            cba: Port::new(io_base + 0x3A),

            transmit_start: [
                Port::new(io_base + 0x20),
                Port::new(io_base + 0x24),
                Port::new(io_base + 0x28),
                Port::new(io_base + 0x2C),
            ],

            transmit_status: [
                Port::new(io_base + 0x10),
                Port::new(io_base + 0x14),
                Port::new(io_base + 0x18),
                Port::new(io_base + 0x1C),
            ],

            tx_buffers: [
                PhysBuf::new(4096),
                PhysBuf::new(4096),
                PhysBuf::new(4096),
                PhysBuf::new(4096),
            ],
            tx_id: AtomicUsize::new(3),

            rx_buffer: PhysBuf::new(9708),
            rx_offset: 0,
        }
    }

    pub fn mac(&self) -> [u8; 6] {
        let mut result = [0; 6];
        unsafe {
            for i in 0..6 {
                let mut port: Port<u8> = Port::new(self.io_base + i as u16);
                result[i] = port.read();
            }
        }
        result
    }

    fn receive_packet(&mut self) -> Option<Vec<u8>> {
        let cmd = unsafe { self.command.read() };
        if (cmd & CR_BUFE) == CR_BUFE {
            return None;
        }

        //let isr = unsafe { self.ports.isr.read() };
        let cba = unsafe { self.cba.read() };
        // CAPR starts at 65520 and with the pad it overflows to 0
        let capr = unsafe { self.capr.read() };
        let offset = ((capr as usize) + RX_BUFFER_PAD) % (1 << 16);
        let header = u16::from_le_bytes(
            self.rx_buffer[(offset + 0)..(offset + 2)]
                .try_into()
                .unwrap(),
        );
        if header & ROK != ROK {
            unsafe {
                self.capr
                    .write((((cba as usize) % RX_BUFFER_LEN) - RX_BUFFER_PAD) as u16)
            };
            return None;
        }

        let n = u16::from_le_bytes(
            self.rx_buffer[(offset + 2)..(offset + 4)]
                .try_into()
                .unwrap(),
        ) as usize;
        //let crc = u32::from_le_bytes(self.rx_buffer[(offset + n)..(offset + n + 4)].try_into().unwrap());

        // Update buffer read pointer
        self.rx_offset = (offset + n + 4 + 3) & !3;
        unsafe {
            self.capr
                .write(((self.rx_offset % RX_BUFFER_LEN) - RX_BUFFER_PAD) as u16);
        }

        //unsafe { self.ports.isr.write(0x1); }
        Some(self.rx_buffer[(offset + 4)..(offset + n)].to_vec())
    }

    fn transmit_packet(&mut self, len: usize) {
        let tx_id = self.tx_id.load(Ordering::SeqCst);
        let mut cmd_port = self.transmit_status[tx_id].clone();
        unsafe {
            // RTL8139 will not transmit packets smaller than 64 bits
            let len = len.max(60); // 60 + 4 bits of CRC

            // Fill in Transmit Status: the size of this packet, the early
            // transmit threshold, and clear OWN bit in TSD (this starts the
            // PCI operation).
            // NOTE: The length of the packet use the first 13 bits (but should
            // not exceed 1792 bytes), and a value of 0x000000 for the early
            // transmit threshold means 8 bytes. So we just write the size of
            // the packet.
            cmd_port.write(0x1FFF & len as u32);

            println!("Awaiting OWN");
            while cmd_port.read() & OWN != OWN {}
            println!("Awaiting TOK");
            while cmd_port.read() & TOK != TOK {}
        }
        //unsafe { self.ports.isr.write(0x4); }
    }

    fn next_tx_buffer(&mut self, len: usize) -> &mut [u8] {
        let tx_id = (self.tx_id.load(Ordering::SeqCst) + 1) % TX_BUFFERS_COUNT;
        self.tx_id.store(tx_id, Ordering::Relaxed);
        &mut self.tx_buffers[tx_id][0..len]
    }
}

static NETWORK_IFACE: OnceCell<Mutex<(Interface, Fuck)>> = OnceCell::uninit();

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

pub fn init() {
    let device = get_device(0x10ec, 0x8139).unwrap();
    let io_base = (device.base_addresses[0] as u16) & 0xFFF0;
    device.enable_mastering();

    //TRANSMIT_BUFFER_1.init_once(|| Vec::with_capacity(4096));
    //TRANSMIT_BUFFER_2.init_once(|| Vec::with_capacity(4096));
    //TRANSMIT_BUFFER_3.init_once(|| Vec::with_capacity(4096));
    //TRANSMIT_BUFFER_4.init_once(|| Vec::with_capacity(4096));

    // let virtual_addr = VirtAddr::new(TRANSMIT_BUFFER_1.get().unwrap() as *const _ as u64);
    // println!("Vec virtual: {virtual_addr:?}");
    // let physical_addr = memory::virt_to_phys(virtual_addr);
    // println!("Vec physical: {physical_addr:?}");

    // let virtual_addr = VirtAddr::new(&TRANSMIT_BUFFER_2 as *const _ as u64);
    // println!("Array virtual: {virtual_addr:?}");
    // let physical_addr = memory::virt_to_phys(virtual_addr);
    // println!("Array physical: {physical_addr:?}");

    let mut rtl = Rtl8139::new(io_base);
    unsafe {
        println!("writing to config");
        rtl.config1.write(0);
        println!("writing to command");
        rtl.command.write(0x10);
        println!("waiting for command");
        while rtl.command.read() & 0x10 != 0 {
            print!(".")
        }
        let mac = rtl.mac().map(|e| format!("{e:x}")).join(":");
        println!("MAC address: {:x?}", mac);
        println!("writing to rbstart");
        rtl.rbstart.write(rtl.rx_buffer.addr() as u32);
        rtl.transmit_start[0].write(rtl.tx_buffers[0].addr() as u32);
        rtl.transmit_start[1].write(rtl.tx_buffers[1].addr() as u32);
        rtl.transmit_start[2].write(rtl.tx_buffers[2].addr() as u32);
        rtl.transmit_start[3].write(rtl.tx_buffers[3].addr() as u32);
        println!("writing to imr");
        rtl.imr.write(0x0005);
        println!("writing to rc");
        rtl.rx_config.write(0xf | (1 << 7));
        println!("Writing to transmit config");
        unsafe {
            rtl.tx_config.write(TCR_IFG | TCR_MXDMA1 | TCR_MXDMA2);
        }
        println!("writing to command");
        rtl.command.write(0x0C);
    }

    let mut config = smoltcp::iface::Config::new();
    let addr = HardwareAddress::Ethernet(EthernetAddress::from_bytes(&rtl.mac()));
    config.hardware_addr = Some(addr);
    let mut fuck = Fuck {
        inner: Arc::new(Mutex::new(rtl)),
    };
    let mut iface = Interface::new(config, &mut fuck);

    iface.update_ip_addrs(|ip_addrs| {
        ip_addrs
            .push(IpCidr::new(IpAddress::v4(10, 0, 2, 15), 24))
            .unwrap();
    });
    iface
        .routes_mut()
        .add_default_ipv4_route(Ipv4Address::new(10, 0, 2, 2))
        .unwrap();

    let icmp_rx_buffer = icmp::PacketBuffer::new(vec![icmp::PacketMetadata::EMPTY], vec![0; 256]);
    let icmp_tx_buffer = icmp::PacketBuffer::new(vec![icmp::PacketMetadata::EMPTY], vec![0; 256]);
    let icmp_socket = icmp::Socket::new(icmp_rx_buffer, icmp_tx_buffer);
    let mut sockets = SocketSet::new(vec![]);
    let icmp_handle = sockets.add(icmp_socket);

    let mut send_at = Instant::from_millis(0);
    let mut seq_no = 0;
    let mut received = 0;
    let mut echo_payload = [0xffu8; 40];
    //let mut waiting_queue = Vec::new();
    let ident = 0x22b;

    loop {
        let timestamp = Instant::from_secs(time::time() as i64);
        iface.poll(timestamp, &mut fuck, &mut sockets);

        let socket = sockets.get_mut::<icmp::Socket>(icmp_handle);
        if !socket.is_open() {
            socket.bind(icmp::Endpoint::Ident(ident)).unwrap();
            send_at = timestamp;
        }

        if socket.can_send() {
            println!("calling send");
            let (icmp_repr, mut icmp_packet) = send_icmp_ping!(
                Icmpv4Repr,
                Icmpv4Packet,
                ident,
                seq_no,
                echo_payload,
                socket,
                IpAddress::v4(10, 0, 2, 2)
            );
            icmp_repr.emit(&mut icmp_packet, &fuck.capabilities().checksum);
        }
    }

    //NETWORK_IFACE.init_once(|| Mutex::new((iface, fuck)));
}

struct Fuck {
    inner: Arc<Mutex<Rtl8139>>,
}

impl phy::Device for Fuck {
    type RxToken<'a> = StmPhyRxToken where Self: 'a;
    type TxToken<'a> = StmPhyTxToken where Self: 'a;

    fn receive(&mut self, _timestamp: Instant) -> Option<(Self::RxToken<'_>, Self::TxToken<'_>)> {
        if let Some(buffer) = self.inner.lock().receive_packet() {
            Some((
                StmPhyRxToken { buffer },
                StmPhyTxToken {
                    device: self.inner.clone(),
                },
            ))
        } else {
            None
        }
    }

    fn transmit(&mut self, _timestamp: Instant) -> Option<Self::TxToken<'_>> {
        Some(StmPhyTxToken {
            device: self.inner.clone(),
        })
    }

    fn capabilities(&self) -> DeviceCapabilities {
        let mut caps = DeviceCapabilities::default();
        caps.max_transmission_unit = 1536;
        caps.max_burst_size = Some(1);
        caps.medium = Medium::Ethernet;
        caps
    }
}

pub struct StmPhyRxToken {
    buffer: Vec<u8>,
}

impl phy::RxToken for StmPhyRxToken {
    fn consume<R, F>(mut self, f: F) -> R
    where
        F: FnOnce(&mut [u8]) -> R,
    {
        let result = f(&mut self.buffer);
        result
    }
}

pub struct StmPhyTxToken {
    device: Arc<Mutex<Rtl8139>>,
}

impl<'a> phy::TxToken for StmPhyTxToken {
    fn consume<R, F>(self, len: usize, f: F) -> R
    where
        F: FnOnce(&mut [u8]) -> R,
    {
        let mut dev = self.device.lock();
        let buf = dev.next_tx_buffer(len);
        let result = f(buf);
        dev.transmit_packet(len);
        result
    }
}
