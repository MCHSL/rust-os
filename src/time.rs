use core::{
    pin::Pin,
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
    task::{Context, Poll},
    time::Duration,
};

use alloc::{sync::Arc, vec::Vec};
use futures_util::{task::AtomicWaker, Future};
use generic_once_cell::Lazy;
use spin::Mutex;
use x86_64::{
    instructions::{interrupts, port::Port},
    structures::idt::InterruptStackFrame,
};

use crate::{
    interrupts::{InterruptIndex, PICS},
    print, println,
};

pub const PIT_FREQUENCY: f64 = 3_579_545.0 / 3.0; // 1_193_181.666 Hz
pub const PIT_DIVIDER: usize = 1193;
pub const PIT_INTERVAL: f64 = (PIT_DIVIDER as f64) / PIT_FREQUENCY;

static CLOCK: AtomicUsize = AtomicUsize::new(0);

pub fn elapsed(start_tick: usize) -> f64 {
    let interval = CLOCK.load(Ordering::Relaxed) - start_tick;
    interval as f64 * PIT_INTERVAL
}

pub fn time() -> f64 {
    CLOCK.load(Ordering::Relaxed) as f64 * PIT_INTERVAL
}

struct SleepsterInner {
    waker: AtomicWaker,
    wake_at: f64,
}

#[derive(Clone)]
pub struct Sleepster {
    _inner: Arc<SleepsterInner>,
}

impl Sleepster {
    fn new(wake_at: f64) -> Self {
        Self {
            _inner: Arc::new(SleepsterInner {
                waker: AtomicWaker::new(),
                wake_at,
            }),
        }
    }
}

impl Future for Sleepster {
    type Output = ();
    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let time = time();
        if time >= self._inner.wake_at {
            return Poll::Ready(());
        }

        self._inner.waker.register(cx.waker());
        Poll::Pending
    }
}

static SLEEPERS: Lazy<Mutex<()>, Mutex<Vec<Sleepster>>> = Lazy::new(|| Mutex::new(Vec::new()));

pub fn sleep(duration: Duration) -> Sleepster {
    let start_time = time();
    let end_time = start_time + duration.as_secs_f64();
    let sleepster = Sleepster::new(end_time);
    SLEEPERS.lock().push(sleepster.clone());
    sleepster
}

pub extern "x86-interrupt" fn timer_interrupt_handler(_stack_frame: InterruptStackFrame) {
    CLOCK.fetch_add(1, Ordering::Relaxed);

    let time = time();
    SLEEPERS.lock().retain(|sleeper| {
        if sleeper._inner.wake_at <= time {
            sleeper._inner.waker.wake();
            false
        } else {
            true
        }
    });

    unsafe {
        PICS.lock()
            .notify_end_of_interrupt(InterruptIndex::Timer.as_u8());
    }
}

pub fn set_pit_frequency_divider(divider: u16, channel: u8) {
    interrupts::without_interrupts(|| {
        let bytes = divider.to_le_bytes();
        let mut cmd: Port<u8> = Port::new(0x43);
        let mut data: Port<u8> = Port::new(0x40 + channel as u16);
        let operating_mode = 6; // Square wave generator
        let access_mode = 3; // Lobyte + Hibyte
        unsafe {
            cmd.write((channel << 6) | (access_mode << 4) | operating_mode);
            data.write(bytes[0]);
            data.write(bytes[1]);
        }
    });
}

pub extern "x86-interrupt" fn rtc_interrupt_handler(stack_frame: InterruptStackFrame) {
    print!("+");
    static SHOWN: AtomicBool = AtomicBool::new(false);
    let val = SHOWN.load(Ordering::Relaxed);
    if !val {
        println!("{:?}", stack_frame);
        SHOWN.store(true, Ordering::Relaxed);
    }

    unsafe {
        PICS.lock()
            .notify_end_of_interrupt(InterruptIndex::RealTimeClock.as_u8());
    }
}
