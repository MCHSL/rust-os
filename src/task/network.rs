use core::{
    pin::Pin,
    sync::atomic::{AtomicBool, Ordering},
    task::{self, Poll},
    time::Duration,
};

use alloc::{sync::Arc, vec::Vec};
use conquer_once::spin::OnceCell;
use futures_util::{future::select, task::AtomicWaker, Future};
use spin::Mutex;
use x86_64::instructions::{hlt, interrupts::without_interrupts};

use crate::{
    drivers::net::{get_interfaces, rtl8139::rtl_receive, SocketRecvWaiterInner, NET_IFACES},
    println,
    time::{sleep, yield_now},
};

pub static RECEIVING_SOCKETS: Mutex<Vec<Arc<SocketRecvWaiterInner>>> = Mutex::new(Vec::new());

pub static TX_WAKER: OnceCell<Arc<WaitForTxInner>> = OnceCell::uninit();

pub struct WaitForTxInner {
    ready: AtomicBool,
    waker: AtomicWaker,
}

impl WaitForTxInner {
    const fn new() -> Self {
        Self {
            ready: AtomicBool::new(false),
            waker: AtomicWaker::new(),
        }
    }
}

struct WaitForTx {
    inner: Arc<WaitForTxInner>,
}

impl Future for WaitForTx {
    type Output = ();
    fn poll(self: Pin<&mut Self>, cx: &mut task::Context<'_>) -> task::Poll<Self::Output> {
        let ready = self.inner.ready.load(Ordering::Relaxed);
        if ready {
            self.inner.ready.store(false, Ordering::Relaxed);
            return Poll::Ready(());
        }
        self.inner.waker.register(cx.waker());
        let ready = self.inner.ready.load(Ordering::Relaxed);
        if ready {
            self.inner.ready.store(false, Ordering::Relaxed);
            return Poll::Ready(());
        }
        Poll::Pending
    }
}

fn wait_for_tx() -> WaitForTx {
    without_interrupts(|| {
        TX_WAKER.init_once(|| Arc::new(WaitForTxInner::new()));
        let waker = TX_WAKER.get().unwrap();
        //waker.ready.store(false, Ordering::Relaxed);
        WaitForTx {
            inner: waker.clone(),
        }
    })
}

pub fn notify_tx() {
    TX_WAKER.init_once(|| Arc::new(WaitForTxInner::new()));
    let tx = TX_WAKER.get().unwrap();
    tx.ready.store(true, Ordering::Relaxed);
    tx.waker.wake();
}

pub fn pump_interfaces() -> bool {
    let mut ifaces = get_interfaces();
    let mut changed = false;
    for iface in ifaces.iter_mut() {
        without_interrupts(|| {
            changed = changed || iface.poll();
        })
    }
    changed
}

pub async fn keep_pumping_interfaces() {
    let mut ifaces = get_interfaces();

    loop {
        //let mut rx = None;
        //let mut tx = None;
        //without_interrupts(|| {
        let mut changed = false;
        for iface in ifaces.iter_mut() {
            changed = changed || iface.poll();
        }

        //println!("Poll result: {changed}");
        if changed {
            for sock in RECEIVING_SOCKETS.lock().drain(..) {
                sock.ready.store(true, Ordering::Relaxed);
                sock.waker.wake();
            }
        }
        //println!("Pump: waiting for rx or tx");
        //rx = Some(rtl_receive());
        //tx = Some(wait_for_tx());
        //});

        //println!("now really waiting");
        select(rtl_receive(), wait_for_tx()).await;
        //println!("Something happened {:?}", core::mem::discriminant(&result));
        //sleep(Duration::from_millis(10)).await;
    }
}
