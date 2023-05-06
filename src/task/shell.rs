use core::time::Duration;

use alloc::{string::String, vec::Vec};
use futures_util::StreamExt;
use pc_keyboard::DecodedKey;

use crate::{backspace, print, println, time::sleep};

use super::{executor::TaskSpawner, keyboard::KeyStream};

pub async fn shell(_spawner: TaskSpawner) {
    let mut stream = KeyStream::new();
    let mut buffer = String::new();
    loop {
        print!("# ");
        loop {
            if let Some(key) = { stream.next().await } {
                match key {
                    DecodedKey::Unicode(key) => {
                        if key == '\n' {
                            break;
                        } else if key == char::from(8) {
                            backspace!();
                            buffer.pop();
                        } else {
                            buffer.push(key);
                            print!("{key}");
                        }
                    }
                    DecodedKey::RawKey(_key) => {
                        //print!("{key:?}");
                    }
                }
            }
        }
        println!("");
        let mut input = buffer.split_ascii_whitespace();
        if let Some(command) = input.next() {
            match command {
                "hello" => {
                    println!("world!");
                }
                "echo" => {
                    let rest = input.collect::<Vec<&str>>().join(" ");
                    println!("{rest}");
                }
                "sleep" => {
                    match input.next() {
                        Some(dur) => match dur.parse() {
                            Ok(dur) => sleep(Duration::from_millis(dur)).await,
                            Err(e) => println!("Error parsing argument: {e}"),
                        },
                        None => println!("Missing argument"),
                    };
                }
                _ => {
                    println!("Unrecognized commmand: {}", command)
                }
            }
        }

        buffer.clear();
    }
}
