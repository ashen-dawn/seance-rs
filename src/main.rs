#![feature(str_split_whitespace_remainder)]

mod config;
mod system;
use crossterm::{cursor::{self, MoveTo}, terminal::{Clear, ClearType, DisableLineWrap, EnableLineWrap, EnterAlternateScreen, LeaveAlternateScreen}};
use system::{Manager, SystemThreadCommand};
use std::{collections::{HashMap, VecDeque}, fs, io::{self, Write}, sync::mpsc, thread::{self, sleep, JoinHandle}, time::Duration};
use tokio::runtime;

pub struct UiState {
    pub systems: HashMap<String, SystemState>,
    pub logs: VecDeque<String>,
}

pub enum SystemState {
    Running(HashMap<String, MemberState>),
    Reloading,
    Restarting,
    Shutdown,
}

pub struct MemberState {
    pub connected: bool,
    pub autoproxied: bool,
}

pub enum SystemUiEvent {
    SystemClose,
    MemberAutoproxy(Option<String>),
    GatewayDisconnect(String),
    GatewayConnect(String),
    LogLine(String),
}

const MAX_LOG : usize = 1000;

fn main() {
    let initial_config = fs::read_to_string("./config.toml").expect("Could not find config file");
    let config = config::Config::load(initial_config.to_string());

    let (waker, waiter) = mpsc::channel::<(String, SystemUiEvent)>();
    let mut join_handles = Vec::<(String, JoinHandle<_>)>::new();

    let mut ui_state = UiState {
        systems: HashMap::new(),
        logs: VecDeque::new(),
    };

    for (system_name, system_config) in config.systems.iter() {
        let handle = spawn_system(system_name, system_config.clone(), waker.clone());

        let mut member_states = HashMap::new();
        for member_name in system_config.members.iter() {
            member_states.insert(member_name.name.clone(), MemberState {
                connected: false,
                autoproxied: false,
            });
        }

        let system_state = SystemState::Running(member_states);
        ui_state.systems.insert(system_name.clone(), system_state);

        join_handles.push((system_name.clone(), handle));
    }

    crossterm::execute!(io::stdout(), EnterAlternateScreen).unwrap();
    crossterm::execute!(io::stdout(), DisableLineWrap).unwrap();

    loop {
        // Wait for an event from one of the threads
        let ui_event = waiter.recv_timeout(Duration::from_millis(500));

        if let Ok((system_name, ui_event)) = ui_event {
            let system_state = ui_state.systems.get_mut(&system_name).unwrap();
            match system_state {
                SystemState::Running(member_states) => match ui_event {
                    // We will check for the join in a second
                    SystemUiEvent::SystemClose => (),

                    SystemUiEvent::MemberAutoproxy(member_name) => {
                        member_states.iter_mut().for_each(|(_, member_state)| {
                            member_state.autoproxied = false;
                        });

                        if let Some(member_name) = member_name {
                            member_states.get_mut(&member_name).unwrap()
                                .autoproxied = true;
                        }
                    },

                    SystemUiEvent::GatewayDisconnect(member_name) => {
                        member_states.get_mut(&member_name).unwrap()
                            .connected = false;
                    },

                    SystemUiEvent::GatewayConnect(member_name) => {
                        member_states.get_mut(&member_name).unwrap()
                            .connected = true;

                    },

                    SystemUiEvent::LogLine(log) => {
                        if log.len() == MAX_LOG {
                            let _ = ui_state.logs.pop_front();
                        }

                        ui_state.logs.push_back(
                            format!("{system_name:>8.8}: {log}")
                        );
                    },

                },
                _ => (),
            }
        }

        // Just to make sure the join handle is updated by the time we check
        sleep(Duration::from_millis(10));

        if let Some(completed_index) = join_handles.iter().position(|(_, handle)| handle.is_finished()) {
            let (name, next_join) = join_handles.swap_remove(completed_index);

            match next_join.join() {
                Err(err) => {
                    let _ = ui_state.systems.insert(name.clone(), SystemState::Shutdown);
                    ui_state.logs.push_back(
                        format!("Thread for system {} panicked!", name)
                    );

                    ui_state.logs.push_back(
                        format!("{:?}", err)
                    );
                },

                Ok(SystemThreadCommand::Restart) => {
                    let _ = ui_state.systems.insert(name.clone(), SystemState::Restarting);
                    ui_state.logs.push_back(
                        format!("Thread for system {} requested restart", name)
                    );
                    if let Some((_, config)) = config.systems.iter().find(|(system_name, _)| name == **system_name) {
                        let handle = spawn_system(&name, config.clone(), waker.clone());
                        join_handles.push((name, handle));
                    }
                },

                Ok(SystemThreadCommand::ShutdownSystem) => {
                    let _ = ui_state.systems.insert(name.clone(), SystemState::Shutdown);
                    ui_state.logs.push_back(
                        format!("Thread for system {} requested shutdown", name)
                    );
                    continue;
                },

                Ok(SystemThreadCommand::ReloadConfig) => {
                    let _ = ui_state.systems.insert(name.clone(), SystemState::Reloading);
                    ui_state.logs.push_back(
                        format!("Thread for system {} requested config reload", name)
                    );
                    let config_file = if let Ok(config_file) = fs::read_to_string("./config.toml") {
                        config_file
                    } else {
                        ui_state.logs.push_back(
                            format!("Could not open config file, continuing with initial config")
                        );
                        initial_config.clone()
                    };

                    let updated_config = config::Config::load(config_file);

                    if let Some((_, system_config)) = updated_config.systems.into_iter().find(|(system_name, _)| *name == *system_name) {
                        let handle = spawn_system(&name, system_config, waker.clone());
                        join_handles.push((name.clone(), handle));
                    } else {
                        ui_state.logs.push_back(
                            format!("New config file but this system no longer exists, exiting.")
                        );
                        continue;
                    }
                },
                Ok(SystemThreadCommand::ShutdownAll) => break,
            }
        }

        update_ui(&ui_state, &config);
    }

    crossterm::execute!(io::stdout(), EnableLineWrap).unwrap();
    crossterm::execute!(io::stdout(), LeaveAlternateScreen).unwrap();
}

fn spawn_system(system_name : &String, system_config: config::System, waker: mpsc::Sender<(String, SystemUiEvent)>) -> JoinHandle<SystemThreadCommand> {
    let name = system_name.clone();
    let config = system_config.clone();

    thread::Builder::new()
        .name(format!("seance_{}", &name))
        .spawn(move || -> _ {
            let thread_local_runtime = runtime::Builder::new_current_thread().enable_all().build().unwrap();
            let dup_waker = waker.clone();

            thread_local_runtime.block_on(async {
                let mut system = Manager::new(name.clone(), config, waker);
                system.start_clients().await;
            });

            let _ = dup_waker.send((name, SystemUiEvent::SystemClose));
            SystemThreadCommand::Restart
        }).unwrap()
}

fn update_ui(ui_state: &UiState, config: &config::Config) {
    crossterm::execute!(io::stdout(), Clear(ClearType::FromCursorUp)).unwrap();
    crossterm::execute!(io::stdout(), MoveTo(0, 0)).unwrap();

    let (width, height) = crossterm::terminal::size().unwrap();

    let status_lines = (ui_state.systems.len() * 2) + ui_state.systems.values().map(|system| match system {
        SystemState::Running(members) => members.len(),
        SystemState::Reloading => 1,
        SystemState::Restarting => 1,
        SystemState::Shutdown => 1,
    } ).sum::<usize>() + 1;

    let log_space = height as usize - status_lines - 1;
    let log_height = ui_state.logs.len();

    for (name, state) in ui_state.systems.iter() {
        println!("{name}");
        match state {
            SystemState::Shutdown => println!("  - [System stopped]"),
            SystemState::Reloading => println!("  - [System reloading]"),
            SystemState::Restarting => println!("  - [System restarting]"),
            SystemState::Running(members) => for (name, state) in members {
                if !state.connected {
                    println!("  - {name} (connecting)")
                } else if state.autoproxied {
                    println!("  - {name} (autoproxy)")
                } else {
                    println!("  - {name}")
                }
            },
        }

        println!("");
    }

    println!("{:-<width$}", "", width = width as usize);

    let range = if log_height <= log_space {
        0..log_height
    } else {
        log_height - log_space .. log_height
    };

    for line in ui_state.logs.range(range) {
        println!("{line}");
    }
}
