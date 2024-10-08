#![feature(str_split_whitespace_remainder)]

mod config;
mod system;
use system::{Manager, SystemThreadCommand};
use std::{fs, thread::{self, sleep, JoinHandle}, time::Duration, sync::mpsc};
use tokio::runtime;

fn main() {
    let initial_config = fs::read_to_string("./config.toml").expect("Could not find config file");
    let config = config::Config::load(initial_config.to_string());

    let (waker, waiter) = mpsc::channel::<()>();
    let mut join_handles = Vec::<(String, JoinHandle<_>)>::new();

    for (system_name, system_config) in config.systems.iter() {
        let handle = spawn_system(system_name, system_config.clone(), waker.clone());

        join_handles.push((system_name.clone(), handle));
    }

    loop {
        // Check manually every 10 seconds just in case
        let _ = waiter.recv_timeout(Duration::from_secs(10));

        // Just to make sure the join handle is updated by the time we check
        sleep(Duration::from_millis(100));

        if let Some(completed_index) = join_handles.iter().position(|(_, handle)| handle.is_finished()) {
            let (name, next_join) = join_handles.swap_remove(completed_index);

            match next_join.join() {
                Err(err) => {
                    println!("Thread for system {} panicked!", name);
                    println!("{:?}", err);
                },

                Ok(SystemThreadCommand::Restart) => {
                    println!("Thread for system {} requested restart", name);
                    if let Some((_, config)) = config.systems.iter().find(|(system_name, _)| name == **system_name) {
                        let handle = spawn_system(&name, config.clone(), waker.clone());
                        join_handles.push((name, handle));
                    }
                },

                Ok(SystemThreadCommand::ShutdownSystem) => {
                    println!("Thread for system {} requested shutdown", name);
                    continue;
                },
                Ok(SystemThreadCommand::ReloadConfig) => {
                    println!("Thread for system {} requested config reload", name);
                    let config_file = if let Ok(config_file) = fs::read_to_string("./config.toml") {
                        config_file
                    } else {
                        println!("Could not open config file, continuing with initial config");
                        initial_config.clone()
                    };

                    let updated_config = config::Config::load(config_file);

                    if let Some((_, system_config)) = updated_config.systems.into_iter().find(|(system_name, _)| *name == *system_name) {
                        let handle = spawn_system(&name, system_config, waker.clone());
                        join_handles.push((name.clone(), handle));
                    } else {
                        println!("New config file but this system no longer exists, exiting.");
                        continue;
                    }
                },
                Ok(SystemThreadCommand::ShutdownAll) => break,
            }
        }
    }
}

fn spawn_system(system_name : &String, system_config: config::System, waker: mpsc::Sender<()>) -> JoinHandle<SystemThreadCommand> {
    let name = system_name.clone();
    let config = system_config.clone();

    thread::spawn(move || -> _ {
        let thread_local_runtime = runtime::Builder::new_current_thread().enable_all().build().unwrap();

        // TODO: allow system manager runtime to return a command
        thread_local_runtime.block_on(async {
            let mut system = Manager::new(name, config);
            system.start_clients().await;
        });

        let _ = waker.send(());
        SystemThreadCommand::Restart
    })
}


