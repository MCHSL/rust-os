use conquer_once::spin::OnceCell;
use smoltcp::iface::SocketSet;
use spin::Mutex;

pub mod icmp;

pub static SOCKETS: OnceCell<Mutex<SocketSet>> = OnceCell::uninit();
