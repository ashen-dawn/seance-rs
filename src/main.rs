mod config;
use config::Config;
use serenity::{all::{GatewayIntents, Message}, client::{Context, EventHandler}, Client};

#[tokio::main(flavor = "current_thread")]
async fn main() {
    println!("Hello, world!");

    let config_str = include_str!("../config.toml");
    let config = Config::load(config_str.to_string());

    let token = config.systems.get("ashe-test").unwrap().members
        .iter().find(|member| member.name == "test").unwrap()
        .discord_token.clone();

    println!("Token: {}", token);

    let mut client = Client::builder(token, GatewayIntents::all())
        .event_handler(MessageHandler).await.unwrap();
        
    client.start().await.unwrap()
}


struct MessageHandler;

#[serenity::async_trait]
impl EventHandler for MessageHandler {
    async fn message(&self, context: Context, msg: Message) {
        if msg.member.unwrap().user.unwrap().bot { // TODO crashes
            return
        }
        
        println!("Got message");
        println!("{}", msg.content);
        if let Ok(_test) = msg.channel_id.say(&context, "message acknowledged").await {
            println!("Reply sent")
        } else {
            println!("Error encountered")
        }
    }
}
