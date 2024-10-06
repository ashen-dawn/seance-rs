#![feature(str_split_whitespace_remainder)]

mod config;
mod system;
use system::Manager;

use std::{fs, panic};
use tokio::{runtime, task::JoinSet};

fn main() {
    let initial_config = fs::read_to_string("./config.toml").expect("Could not find config file");
    let config = config::Config::load(initial_config.to_string());

    let waiter_pool = runtime::Builder::new_multi_thread()
        .worker_threads(config.systems.len()).build().unwrap();
    let mut waiters = JoinSet::new();

    for (system_name, system_config) in config.systems.into_iter() {
        spawn_system(&mut waiters, &waiter_pool, &system_name, system_config);
    }

    runtime::Builder::new_current_thread().build().unwrap().block_on(async {
        while let Some(system_join) = waiters.join_next().await {
            if let Ok(system_name) = system_join {
                println!("Thread joined for system: {}. Updating config and restarting.", system_name);

                let config_file = if let Ok(config_file) = fs::read_to_string("./config.toml") {
                    config_file
                } else {
                    println!("Could not open config file, continuing with initial config");
                    initial_config.clone()
                };

                let updated_config = config::Config::load(config_file);

                if let Some((_, system_config)) = updated_config.systems.into_iter().find(|(name, _)| name.eq(&system_name)) {
                    spawn_system(&mut waiters, &waiter_pool, &system_name, system_config.clone());
                } else {
                    println!("New config file but this system no longer exists, exiting.");
                }
            } else {
                println!("Thread panicked");
            }
        }
    })
}

fn spawn_system(joinset: &mut JoinSet<String>, pool: &tokio::runtime::Runtime, system_name : &String, system_config: config::System) {
    let name = system_name.clone();
    let config = system_config.clone();
    joinset.spawn_blocking_on(move || -> String {
        let thread_local_runtime = runtime::Builder::new_current_thread().enable_all().build().unwrap();

        let _ = panic::catch_unwind(|| {
            thread_local_runtime.block_on(async {
                let mut system = Manager::new(name.clone(), config);
                system.start_clients().await;
            });
        });

        name
    }, pool.handle());
}


