mod config;
mod system;
mod listener;

use std::thread::{self, JoinHandle};
use tokio::runtime;

use system::System;

fn main() {
    let config_str = include_str!("../config.toml");
    let config = config::Config::load(config_str.to_string());

    let handles : Vec<_> = config.systems.into_iter().map(|(system_name, system_config)| -> JoinHandle<()>  {
        thread::spawn(move || {
            let runtime = runtime::Builder::new_current_thread().enable_all().build().expect("Could not construct Tokio runtime");
            runtime.block_on(async {
                let mut system = System::new(system_name, system_config);
                system.start_clients().await;
            })
        })
    }).collect();

    for thread_handle in handles.into_iter() {
        thread_handle.join().expect("Child thread has panicked");
    }
}


