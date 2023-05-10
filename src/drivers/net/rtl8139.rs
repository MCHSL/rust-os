use core::{
    hint::spin_loop,
    pin::Pin,
    sync::atomic::{AtomicBool, AtomicU16, AtomicUsize, Ordering},
    task::{Context, Poll},
};

use alloc::{sync::Arc, vec::Vec};
use conquer_once::spin::OnceCell;
use crossbeam_queue::ArrayQueue;
use futures_util::{task::AtomicWaker, Future};
use smoltcp::{
    phy::{DeviceCapabilities, Medium},
    wire::{EthernetAddress, HardwareAddress},
};
use spin::Mutex;
use x86_64::{
    instructions::{
        hlt,
        interrupts::{self, without_interrupts},
        port::Port,
    },
    structures::idt::{InterruptDescriptorTable, InterruptStackFrame},
};

use crate::{
    allocator::PhysBuf,
    hlt_loop,
    interrupts::{IDT, PICS},
    pci, print, println,
    task::network::notify_tx,
};

use super::EthernetDevice;

const RST: u8 = 1 << 4; // Reset
const RE: u8 = 1 << 3; // Receiver enable
const TE: u8 = 1 << 2; // Transmitter enable

const ROK: u16 = 1 << 0; // Receive OK
const BUFE: u8 = 1 << 0; // RX buffer empty

const TOK: u32 = 1 << 15; // Transmit OK
const OWN: u32 = 1 << 13; // Transmit DMA complete

const AAP: u32 = 1 << 0; // Accept All Packets
const APM: u32 = 1 << 1; // Accept Physical Match Packets
const AM: u32 = 1 << 2; // Accept Multicast Packets
const AB: u32 = 1 << 3; // Accept Broadcast Packets
const AR: u32 = 1 << 4; // Accept Runt Packets
const AER: u32 = 1 << 5; // Accept Error Packets
const WRAP: u32 = 1 << 7; // 1: Write past end of buffer

const RX_BUFFER_WRAP_SPACE: usize = 1500;
const RX_BUFFER_PADDING: usize = 16;
const RX_BUFFER_LEN: usize = 8192 + RX_BUFFER_WRAP_SPACE + RX_BUFFER_PADDING;

const TCR_IFG: u32 = 3 << 24;
const TCR_MXDMA1: u32 = 1 << 9;
const TCR_MXDMA2: u32 = 1 << 10;

pub struct Rtl8139 {
    io_base: u16,
    config1: Port<u8>,
    command: Port<u8>,
    rx_buffer_port: Port<u32>,
    imr: Port<u16>,
    isr: Port<u16>,
    tx_config: Port<u32>,
    rx_config: Port<u32>,
    capr: Port<u16>,
    rx_offset_port: Port<u16>,

    tx_buffer_ports: [Port<u32>; 4],
    tx_status_ports: [Port<u32>; 4],

    tx_buffers: [PhysBuf; 4],
    current_tx_buffer: AtomicUsize,
    rx_buffer: PhysBuf,
    rx_offset: usize,
}

impl Rtl8139 {
    pub fn new(io_base: u16) -> Self {
        RTL_IO_BASE.store(io_base, Ordering::Relaxed);
        let mut this = Self {
            io_base,
            config1: Port::new(io_base + 0x52),
            command: Port::new(io_base + 0x37),
            rx_buffer_port: Port::new(io_base + 0x30),
            imr: Port::new(io_base + 0x3C),
            isr: Port::new(io_base + 0x3E),
            tx_config: Port::new(io_base + 0x40),
            rx_config: Port::new(io_base + 0x44),
            capr: Port::new(io_base + 0x38),
            rx_offset_port: Port::new(io_base + 0x3A),

            tx_buffer_ports: [
                Port::new(io_base + 0x20),
                Port::new(io_base + 0x24),
                Port::new(io_base + 0x28),
                Port::new(io_base + 0x2C),
            ],

            tx_status_ports: [
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
            current_tx_buffer: AtomicUsize::new(3),

            rx_buffer: PhysBuf::new(9708),
            rx_offset: 0,
        };

        this.init();
        this
    }

    pub fn init(&mut self) {
        unsafe {
            // Power on
            self.config1.write(0);

            // Reset and wait until reset completes
            self.command.write(RST);
            while self.command.read() & RST != 0 {
                spin_loop();
            }

            // Set physical addresses of our packet buffers
            self.rx_buffer_port.write(self.rx_buffer.addr() as u32);
            self.tx_buffer_ports[0].write(self.tx_buffers[0].addr() as u32);
            self.tx_buffer_ports[1].write(self.tx_buffers[1].addr() as u32);
            self.tx_buffer_ports[2].write(self.tx_buffers[2].addr() as u32);
            self.tx_buffer_ports[3].write(self.tx_buffers[3].addr() as u32);

            // Accept only Transmit OK and Receive OK interrupts
            self.imr.write(0x5);
            //self.isr.write(0x5);

            let irq_num = pci::get_device(0x10EC, 0x8139).unwrap().read(0xF).byte(0);
            println!("IRQ: {irq_num}");
            let mut idt = IDT.get().unwrap().lock();
            idt[32 + irq_num as usize].set_handler_fn(rtl8139_handler);

            // Accept all packets and write them past the end of the receive buffer
            self.rx_config.write(AB | AM | APM | AAP | WRAP);

            // Enable TX and RX
            self.command.write(TE | RE);
        }
    }
}

static RTL_IO_BASE: AtomicU16 = AtomicU16::new(0);
pub static RX_WAKER: OnceCell<Arc<Rtl8139RecvInner>> = OnceCell::uninit();

pub struct Rtl8139RecvInner {
    ready: AtomicBool,
    waker: AtomicWaker,
}

pub struct Rtl8139Recv {
    inner: Arc<Rtl8139RecvInner>,
}

impl Future for Rtl8139Recv {
    type Output = ();
    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        without_interrupts(|| {
            let ready = self.inner.ready.load(Ordering::Relaxed);

            if ready {
                self.inner.ready.store(false, Ordering::Relaxed);
                Poll::Ready(())
            } else {
                self.inner.waker.register(cx.waker());
                Poll::Pending
            }
        })
    }
}

pub fn rtl_receive() -> Rtl8139Recv {
    RX_WAKER.init_once(|| {
        Arc::new(Rtl8139RecvInner {
            ready: AtomicBool::new(false),
            waker: AtomicWaker::new(),
        })
    });
    //without_interrupts(|| RTL_WAITERS.lock().push(waiter.clone()));
    Rtl8139Recv {
        inner: RX_WAKER.get().unwrap().clone(),
    }
}

pub fn notify_rx() {
    RX_WAKER.init_once(|| {
        Arc::new(Rtl8139RecvInner {
            ready: AtomicBool::new(false),
            waker: AtomicWaker::new(),
        })
    });
    without_interrupts(|| {
        let waker = RX_WAKER.get().unwrap();
        waker.ready.store(true, Ordering::Relaxed);
        waker.waker.wake();
    })
}

//static RTL_WAITERS: Mutex<Vec<Arc<Rtl8139RecvInner>>> = Mutex::new(Vec::new());

extern "x86-interrupt" fn rtl8139_handler(stack_frame: InterruptStackFrame) {
    let mut imr: Port<u16> = Port::new(RTL_IO_BASE.load(Ordering::Relaxed) + 0x3C);
    let mut isr: Port<u16> = Port::new(RTL_IO_BASE.load(Ordering::Relaxed) + 0x3E);

    let status = unsafe {
        let s = isr.read();
        isr.write(s);
        s
    };

    if (status & ROK) == ROK {
        notify_rx();
    }

    if (status & 0x4) == 0x4 {
        //notify_tx();
    }

    //println!("interrupt complete");

    unsafe {
        PICS.lock().notify_end_of_interrupt(43);
    }
}

impl EthernetDevice for Rtl8139 {
    fn mac(&self) -> HardwareAddress {
        let mut result = [0; 6];
        unsafe {
            for i in 0..6 {
                let mut port: Port<u8> = Port::new(self.io_base + i as u16);
                result[i] = port.read();
            }
        }
        HardwareAddress::Ethernet(EthernetAddress(result))
    }

    fn get_capabilities(&self) -> DeviceCapabilities {
        let mut caps = DeviceCapabilities::default();
        caps.max_transmission_unit = 1536;
        caps.max_burst_size = Some(1);
        caps.medium = Medium::Ethernet;
        caps
    }

    fn get_transmit_buffer(&mut self, len: usize) -> &mut [u8] {
        let current_tx = (self.current_tx_buffer.load(Ordering::SeqCst) + 1) % 4;
        self.current_tx_buffer.store(current_tx, Ordering::Relaxed);
        &mut self.tx_buffers[current_tx][0..len]
    }

    fn transmit_packet(&mut self, len: usize) {
        //println!("Transmit");
        unsafe {
            let len = len.max(60);
            let current_tx = self.current_tx_buffer.load(Ordering::SeqCst);
            let mut port = self.tx_status_ports[current_tx].clone();

            port.write(0x1FFF & len as u32);
            while port.read() & OWN != OWN {}
            //println!("OWN complete");
            while port.read() & TOK != TOK {}
            //println!("TOK complete");
        }
    }

    fn receive_packet(&mut self) -> Option<alloc::vec::Vec<u8>> {
        let cmd = unsafe { self.command.read() };
        if (cmd & BUFE) == BUFE {
            return None;
        }

        let rx_offset = unsafe { self.rx_offset_port.read() };
        let capr = unsafe { self.capr.read() };
        let offset = ((capr as usize) + RX_BUFFER_PADDING) % (1 << 16);
        let header = u16::from_le_bytes(self.rx_buffer[offset..offset + 2].try_into().unwrap());
        if header & ROK != ROK {
            unsafe {
                self.capr
                    .write((((rx_offset as usize) % RX_BUFFER_LEN) - RX_BUFFER_PADDING) as u16)
            };
            return None;
        }

        let n = u16::from_le_bytes(
            self.rx_buffer[(offset + 2)..(offset + 4)]
                .try_into()
                .unwrap(),
        ) as usize;

        self.rx_offset = (offset + n + 4 + 3) & !3;
        unsafe {
            self.capr
                .write(((self.rx_offset % RX_BUFFER_LEN) - RX_BUFFER_PADDING) as u16);
        }

        //unsafe { self.ports.isr.write(0x1); }
        Some(self.rx_buffer[(offset + 4)..(offset + n)].to_vec())
    }
}
