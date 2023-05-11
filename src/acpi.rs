use core::ptr::NonNull;

use acpi::{AcpiTables, InterruptModel};
use x86_64::PhysAddr;

use crate::{memory, println};

#[derive(Clone)]
struct Handler;

impl acpi::AcpiHandler for Handler {
    unsafe fn map_physical_region<T>(
        &self,
        physical_address: usize,
        size: usize,
    ) -> acpi::PhysicalMapping<Self, T> {
        let virtual_address = memory::phys_to_virt(PhysAddr::new(physical_address as u64));
        acpi::PhysicalMapping::new(
            physical_address,
            NonNull::new(virtual_address.as_mut_ptr()).unwrap(),
            size,
            size,
            Self,
        )
    }

    fn unmap_physical_region<T>(_region: &acpi::PhysicalMapping<Self, T>) {}
}

pub fn read_acpi() {
    let table = unsafe { AcpiTables::search_for_rsdp_bios(Handler).unwrap() };
    let info = table.platform_info().unwrap();
    if let InterruptModel::Apic(apic) = info.interrupt_model {
        println!("{:?}", apic);
    } else {
        println!("aint no apic");
    }
}
