use conquer_once::spin::OnceCell;
use smoltcp::iface::SocketSet;
use spin::Mutex;

pub mod icmp;
pub mod tcp;

pub static SOCKETS: OnceCell<Mutex<SocketSet>> = OnceCell::uninit();

trait Socket {
    type DataType;
}
