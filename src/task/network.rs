use core::{
    pin::Pin,
    sync::atomic::{AtomicBool, Ordering},
    task::{Context, Poll},
};

use alloc::{sync::Arc, vec::Vec};
use conquer_once::spin::OnceCell;
use futures_util::{future::select, task::AtomicWaker, Future};
use spin::Mutex;
use x86_64::instructions::interrupts::without_interrupts;

use crate::networking::get_interfaces;

pub static RECEIVING_SOCKETS: Mutex<Vec<Arc<NotificationWaiterInner>>> = Mutex::new(Vec::new());

pub static TX_WAKER: OnceCell<Arc<NotificationWaiterInner>> = OnceCell::uninit();
pub static RX_WAKER: OnceCell<Arc<NotificationWaiterInner>> = OnceCell::uninit();

pub struct NotificationWaiterInner {
    ready: AtomicBool,
    waker: AtomicWaker,
}

impl NotificationWaiterInner {
    pub fn new() -> Self {
        Self {
            ready: AtomicBool::new(false),
            waker: AtomicWaker::new(),
        }
    }

    pub fn notify(&self) {
        self.ready.store(true, Ordering::Relaxed);
        self.waker.wake();
    }
}

pub struct NotificationWaiter {
    pub inner: Arc<NotificationWaiterInner>,
}

impl Future for NotificationWaiter {
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

fn wait_for_tx() -> NotificationWaiter {
    without_interrupts(|| {
        TX_WAKER.init_once(|| Arc::new(NotificationWaiterInner::new()));
        let waker = TX_WAKER.get().unwrap();
        NotificationWaiter {
            inner: waker.clone(),
        }
    })
}

fn wait_for_rx() -> NotificationWaiter {
    without_interrupts(|| {
        RX_WAKER.init_once(|| Arc::new(NotificationWaiterInner::new()));
        let waker = RX_WAKER.get().unwrap();
        NotificationWaiter {
            inner: waker.clone(),
        }
    })
}

pub fn notify_tx() {
    TX_WAKER.init_once(|| Arc::new(NotificationWaiterInner::new()));
    TX_WAKER.get().unwrap().notify();
}

pub fn notify_rx() {
    RX_WAKER.init_once(|| Arc::new(NotificationWaiterInner::new()));
    RX_WAKER.get().unwrap().notify();
}

pub async fn pump_interfaces() {
    loop {
        let mut ifaces = get_interfaces();
        let mut changed = false;
        for iface in ifaces.iter_mut() {
            changed = changed || iface.poll();
        }

        if changed {
            for sock in RECEIVING_SOCKETS.lock().drain(..) {
                sock.notify();
            }
        }

        select(wait_for_rx(), wait_for_tx()).await;
    }
}
