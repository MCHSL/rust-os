use crate::print;
use alloc::{collections::BTreeMap, sync::Arc};
use conquer_once::spin::OnceCell;
use core::{
    pin::Pin,
    sync::atomic::{AtomicUsize, Ordering},
    task::{Context, Poll},
};
use crossbeam_queue::{ArrayQueue, PopError};
use futures_util::stream::Stream;
use futures_util::stream::StreamExt;
use futures_util::task::AtomicWaker;
use pc_keyboard::{layouts, DecodedKey, HandleControl, Keyboard, ScancodeSet1};
use spin::Mutex;

static SCANCODE_QUEUE: OnceCell<ArrayQueue<u8>> = OnceCell::uninit();
static KEY_STREAMS: OnceCell<Mutex<BTreeMap<usize, Arc<KeyStreamInner>>>> = OnceCell::uninit();
static WAKER: AtomicWaker = AtomicWaker::new();

use crate::println;

/// Called by the keyboard interrupt handler
///
/// Must not block or allocate.
pub(crate) fn add_scancode(scancode: u8) {
    if let Ok(queue) = SCANCODE_QUEUE.try_get() {
        if queue.push(scancode).is_err() {
            println!("WARNING: scancode queue full; dropping keyboard input");
        } else {
            WAKER.wake();
        }
    } else {
        println!("WARNING: scancode queue uninitialized");
    }
}

pub struct ScancodeStream {
    _private: (),
}

impl ScancodeStream {
    pub fn new() -> Self {
        ScancodeStream { _private: () }
    }
}

impl Default for ScancodeStream {
    fn default() -> Self {
        Self::new()
    }
}

impl Stream for ScancodeStream {
    type Item = u8;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context) -> Poll<Option<u8>> {
        let queue = SCANCODE_QUEUE
            .try_get()
            .expect("scancode queue not initialized");

        // fast path
        if let Ok(scancode) = queue.pop() {
            return Poll::Ready(Some(scancode));
        }

        WAKER.register(cx.waker());
        match queue.pop() {
            Ok(scancode) => {
                WAKER.take();
                Poll::Ready(Some(scancode))
            }
            Err(crossbeam_queue::PopError) => Poll::Pending,
        }
    }
}

pub async fn forward_keys() {
    let mut scancodes = ScancodeStream::new();
    let mut keyboard = Keyboard::new(
        ScancodeSet1::new(),
        layouts::Us104Key,
        HandleControl::Ignore,
    );

    while let Some(scancode) = scancodes.next().await {
        if let Ok(Some(key_event)) = keyboard.add_byte(scancode) {
            if let Some(key) = keyboard.process_keyevent(key_event) {
                let queues = KEY_STREAMS
                    .get()
                    .expect("Key receivers not initialized")
                    .lock();
                for holder in queues.values() {
                    holder.queue.push(key).expect("Failed to push key");
                    holder.waker.wake();
                }
            }
        }
    }
}

pub struct KeyStreamInner {
    queue: ArrayQueue<DecodedKey>,
    waker: AtomicWaker,
}

pub struct KeyStream {
    id: usize,
    _inner: Arc<KeyStreamInner>,
}

impl KeyStream {
    pub fn new() -> Self {
        let arc_stream = Arc::new(KeyStreamInner {
            queue: ArrayQueue::new(100),
            waker: AtomicWaker::new(),
        });

        static NEXT_ID: AtomicUsize = AtomicUsize::new(0);
        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        KEY_STREAMS
            .get()
            .expect("Key streams not initialized")
            .lock()
            .insert(id, arc_stream.clone());

        KeyStream {
            id,
            _inner: arc_stream,
        }
    }
}

impl Default for KeyStream {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for KeyStream {
    fn drop(&mut self) {
        KEY_STREAMS
            .get()
            .expect("Key streams not initialized")
            .lock()
            .remove(&self.id);
    }
}

impl Stream for KeyStream {
    type Item = DecodedKey;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context) -> Poll<Option<DecodedKey>> {
        if let Ok(key) = self._inner.queue.pop() {
            return Poll::Ready(Some(key));
        }

        self._inner.waker.register(cx.waker());
        match self._inner.queue.pop() {
            Ok(key) => {
                self._inner.waker.take();
                Poll::Ready(Some(key))
            }
            Err(PopError) => Poll::Pending,
        }
    }
}

pub fn initialize_streams() {
    SCANCODE_QUEUE
        .try_init_once(|| ArrayQueue::new(100))
        .expect("ScancodeStream::new should only be called once");

    KEY_STREAMS
        .try_init_once(|| Mutex::new(BTreeMap::new()))
        .expect("ScancodeStream::new should only be called once");
}

pub async fn print_keys() {
    let mut stream = KeyStream::new();
    loop {
        if let Some(key) = { stream.next().await } {
            match key {
                DecodedKey::Unicode(key) => print!("{key}"),
                DecodedKey::RawKey(key) => print!("{key:?}"),
            }
        }
    }
}
