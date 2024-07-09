use std::collections::HashMap;

use regex::Regex;
use serde::Deserialize;

#[derive(Deserialize)]
pub enum AutoProxyScope {
    Global,
    Server,
    Channel
}

#[derive(Deserialize, Clone)]
pub enum PresenceMode {
    Online,
    Busy,
    Idle,
    Invisible,
}

#[derive(Deserialize, Clone)]
#[serde(tag = "mode", rename_all = "lowercase")]
pub enum AutoproxyConfig {
    Member {
        name: MemberName
    },
    Latch {
        scope: AutoproxyLatchScope,
        timeout_seconds: u32,
        presence_indicator: bool
    }
}

#[derive(Deserialize, Clone)]
#[serde(rename_all = "lowercase")]
pub enum AutoproxyLatchScope {
    Global,
    Server
}

#[derive(Deserialize, Clone)]
pub struct PluralkitConfig {
    #[serde(with = "serde_regex")]
    pub message_pattern: Regex,
    pub api_token: String,
}

#[derive(Deserialize, Clone)]
pub struct System {
    pub reference_user_id: String,
    pub members: Vec<Member>,
    #[serde(default = "default_forward_pings")]
    pub forward_pings: bool,
    pub autoproxy: Option<AutoproxyConfig>,
    pub pluralkit: Option<PluralkitConfig>,
}

fn default_forward_pings() -> bool {
    false
}

pub type MemberName = String;

#[derive(Deserialize, Clone)]
pub struct Member {
    pub name: MemberName,
    #[serde(with = "serde_regex")]
    pub message_pattern: Regex,
    pub discord_token: String,
    pub presence: Option<PresenceMode>,
    pub status: Option<String>,
}

#[derive(Deserialize)]
pub struct Config {
    #[serde(flatten)]
    pub systems: HashMap<String, System>
}

impl Config {
    pub fn load(config_contents: String) -> Config {
        let config : Config = toml::from_str(config_contents.as_str()).unwrap();

        config.systems.iter().for_each(|config_system| {
            let (system_name, system) = config_system;
            if let Some(autoproxy) = &system.autoproxy {
                if let AutoproxyConfig::Member { name } = autoproxy {
                    let member_matches = system.members.iter().all(|member| {
                        member.name == *name
                    });

                    if !member_matches {
                        panic!("System {} autoproxy member {} does not match a known member name", system_name, name);
                    }
                }
            }
        });

        return config
    }
}
