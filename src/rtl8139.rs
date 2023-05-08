use alloc::{format, string::ToString, vec::Vec};
use conquer_once::spin::OnceCell;
use x86_64::{
    instructions::{hlt, port::Port},
    VirtAddr,
};

use crate::{memory, pci::get_device, print, println};

struct Rtl8139 {
    io_base: u16,
    config1: Port<u8>,
    command: Port<u8>,
    rbstart: Port<u32>,
    imr: Port<u16>,
    rcr: Port<u32>,

    transmit_start: [Port<u32>; 4],
    transmit_status: [Port<u32>; 4],
}

impl Rtl8139 {
    pub fn new(io_base: u16) -> Self {
        Self {
            io_base,
            config1: Port::new(io_base + 0x52),
            command: Port::new(io_base + 0x37),
            rbstart: Port::new(io_base + 0x30),
            imr: Port::new(io_base + 0x3C),
            rcr: Port::new(io_base + 0x44),

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
}

static TRANSMIT_BUFFER_1: [u8; 4096] = [0; 4096];
static TRANSMIT_BUFFER_2: [u8; 4096] = [0; 4096];
static TRANSMIT_BUFFER_3: [u8; 4096] = [0; 4096];
static TRANSMIT_BUFFER_4: [u8; 4096] = [0; 4096];

static RECEIVE_BUFFER: [u8; 9708] = [0; 9708];

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
        rtl.rbstart.write(
            memory::virt_to_phys(VirtAddr::new(&RECEIVE_BUFFER as *const _ as u64))
                .unwrap()
                .as_u64() as u32,
        );
        rtl.transmit_start[0].write(
            memory::virt_to_phys(VirtAddr::new(&TRANSMIT_BUFFER_1 as *const _ as u64))
                .unwrap()
                .as_u64() as u32,
        );
        rtl.transmit_start[1].write(
            memory::virt_to_phys(VirtAddr::new(&TRANSMIT_BUFFER_2 as *const _ as u64))
                .unwrap()
                .as_u64() as u32,
        );
        rtl.transmit_start[2].write(
            memory::virt_to_phys(VirtAddr::new(&TRANSMIT_BUFFER_3 as *const _ as u64))
                .unwrap()
                .as_u64() as u32,
        );
        rtl.transmit_start[3].write(
            memory::virt_to_phys(VirtAddr::new(&TRANSMIT_BUFFER_4 as *const _ as u64))
                .unwrap()
                .as_u64() as u32,
        );
        println!("writing to imr");
        rtl.imr.write(0x0005);
        println!("writing to rc");
        rtl.rcr.write(0xf | (1 << 7));
        println!("writing to command");
        rtl.command.write(0x0C);
    }
}
