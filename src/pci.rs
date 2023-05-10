use core::fmt;

use alloc::vec::Vec;
use conquer_once::spin::OnceCell;
use spin::Mutex;
use x86_64::instructions::port::Port;

use crate::println;

pub struct Register {
    inner: u32,
}

impl Register {
    pub fn dword(&self) -> u32 {
        self.inner
    }

    pub fn word(&self, which: u8) -> u16 {
        (self.inner >> (16 * which)) as u16
    }

    pub fn byte(&self, which: u8) -> u8 {
        (self.inner >> (8 * which)) as u8
    }
}

#[derive(Clone)]
pub struct PciDevice {
    pub bus: u8,
    pub slot: u8,
    pub function: u8,

    pub vendor: u16,
    pub id: u16,

    pub revision: u8,
    pub prog: u8,
    pub class: u8,
    pub subclass: u8,

    pub base_addresses: [u32; 6],
}

impl PciDevice {
    fn new(bus: u8, slot: u8, function: u8) -> Self {
        let (id, vendor) = {
            let data = pci_read(bus, slot, 0, 0);
            (data.word(1), data.word(0))
        };
        let (class, subclass, prog, revision) = {
            let data = pci_read(bus, slot, 0, 2);
            (data.byte(3), data.byte(2), data.byte(1), data.byte(0))
        };

        let mut base_addresses = [0u32; 6];
        for i in 0..6 {
            base_addresses[i] = pci_read(bus, slot, 0, (0x4 + i) as u8).dword();
        }

        Self {
            bus,
            slot,
            function,
            id,
            vendor,
            class,
            subclass,
            revision,
            prog,
            base_addresses,
        }
    }

    pub fn write(&self, register: u8, data: Register) {
        pci_write(self.bus, self.slot, self.function, register, data)
    }

    pub fn read(&self, register: u8) -> Register {
        pci_read(self.bus, self.slot, self.function, register)
    }

    pub fn enable_mastering(&self) {
        let mut command = pci_read(self.bus, self.slot, 0, 1).dword();
        command |= 0x4;
        let command = Register { inner: command };
        self.write(1, command);
    }

    pub fn io_base(&self) -> u16 {
        self.enable_mastering();
        (self.base_addresses[0] as u16) & 0xFFF0
    }
}

impl fmt::Debug for PciDevice {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "PCI {}:{} ID: 0x{:x} Vendor: 0x{:x} Class: {:x}:{:x}",
            self.bus, self.slot, self.id, self.vendor, self.class, self.subclass
        )
    }
}

static PCI_DEVICES: OnceCell<Mutex<Vec<PciDevice>>> = OnceCell::uninit();

pub fn scan_devices() {
    if PCI_DEVICES.is_initialized() {
        return;
    }
    PCI_DEVICES.init_once(|| Mutex::new(Vec::new()));
    for bus in 0..255 {
        for slot in 0..32 {
            if let Some(dev) = check_device(bus, slot) {
                println!("PCI device found: {dev:?}");
                PCI_DEVICES.get().unwrap().lock().push(dev);
            }
        }
    }
}

pub fn check_device(bus: u8, slot: u8) -> Option<PciDevice> {
    let vendor = pci_read(bus, slot, 0, 0).word(1);
    if vendor == 0xFFFF {
        return None;
    }
    Some(PciDevice::new(bus, slot, 0))
}

fn pci_read(bus: u8, slot: u8, func: u8, register: u8) -> Register {
    let bus = bus as u32;
    let slot = slot as u32;
    let func = func as u32;
    let offset = register * 4;

    let address = (bus << 16) | (slot << 11) | (func << 8) | (offset & 0xFC) as u32 | 0x80000000;

    let mut control: Port<u32> = Port::new(0xCF8);
    let mut data: Port<u32> = Port::new(0xCFC);

    unsafe {
        control.write(address);
        Register { inner: data.read() }
    }
}

fn pci_write(bus: u8, slot: u8, func: u8, register: u8, data: Register) {
    let bus = bus as u32;
    let slot = slot as u32;
    let func = func as u32;
    let offset = register * 4;

    let address = (bus << 16) | (slot << 11) | (func << 8) | (offset & 0xFC) as u32 | 0x80000000;

    let mut control: Port<u32> = Port::new(0xCF8);
    let mut data_port: Port<u32> = Port::new(0xCFC);

    unsafe {
        control.write(address);
        data_port.write(data.dword());
    }
}

pub fn get_device(vendor: u16, id: u16) -> Option<PciDevice> {
    for device in PCI_DEVICES.get().unwrap().lock().iter() {
        if device.vendor == vendor && device.id == id {
            return Some(device.clone());
        }
    }
    None
}
