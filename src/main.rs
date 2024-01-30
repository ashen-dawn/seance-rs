mod config;
use config::Config;

fn main() {
    println!("Hello, world!");

    let config_str = include_str!("../config.toml");
    let config = Config::load(config_str.to_string());
}
