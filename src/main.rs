mod config;
use config::Config;
use twilight_gateway::{Intents, Shard, ShardId};

#[tokio::main(flavor = "current_thread")]
async fn main() {
    println!("Hello, world!");

    let config_str = include_str!("../config.toml");
    let config = Config::load(config_str.to_string());

    let token = config.systems.get("ashe-test").unwrap().members
        .iter().find(|member| member.name == "test").unwrap()
        .discord_token.clone();

    println!("Token: {}", token);

    let intents = Intents::GUILD_MEMBERS | Intents::GUILD_PRESENCES | Intents::GUILD_MESSAGES | Intents::MESSAGE_CONTENT;

    let mut shard = Shard::new(ShardId::ONE, token, intents);

    loop {
        let event = match shard.next_event().await {
            Ok(event) => event,
            Err(source) => {
                println!("error receiving event");

                if source.is_fatal() {
                    break;
                }

                continue;
            }
        };

        match event {
            twilight_gateway::Event::MessageCreate(message) => println!("Message: {:?}", message),
            twilight_gateway::Event::MessageUpdate(_) => println!("Message updated"),
            twilight_gateway::Event::Ready(_) => println!("Bot ready!"),
            _ => (),
        }
    }
}


