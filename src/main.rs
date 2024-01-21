use std::collections::hash_map::DefaultHasher;
use std::sync::Arc;
use std::{env, hash::Hasher};

use anyhow::Context as _;
use rspotify::scopes;
use rusqlite::Connection;
use serenity::all::{ApplicationId, CommandDataOptionValue};
use serenity::async_trait;
use serenity::model::application::Command;
use serenity::model::prelude::Interaction;
use serenity::model::prelude::{ChannelPinsUpdateEvent, Presence};
use serenity::prelude::{Context, EventHandler};
use serenity::{
    model::application::CommandDataOption, model::channel::Message, prelude::GatewayIntents,
};
// use youtube::Youtube;

use serenity_command_handler::Handler;

use acquiring_taste::AcquiringTaste;
use forms::Forms;
use serenity_command_handler::modules::{spotify, ModLp, ModPoll, Pinboard, SpotifyOAuth};
use spotify_activity::SpotifyActivity;

mod acquiring_taste;
mod complete;
mod forms;
mod spotify_activity;
// mod youtube;
mod lp;

pub fn get_str_opt_ac<'a>(options: &'a [CommandDataOption], name: &str) -> Option<&'a str> {
    options
        .iter()
        .find(|opt| opt.name == name)
        .and_then(|opt| opt.value.as_str())
}

pub fn get_focused_option(options: &[CommandDataOption]) -> Option<&str> {
    options.iter().find_map(|opt| {
        if let CommandDataOptionValue::Autocomplete { value, .. } = &opt.value {
            Some(value.as_str())
        } else {
            None
        }
    })
}

#[derive(Eq, PartialEq)]
enum CompletionType {
    Albums,
    Songs,
}

struct HandlerWrapper(Handler);

#[async_trait]
impl EventHandler for HandlerWrapper {
    async fn ready(&self, ctx: Context, data_about_bot: serenity::model::gateway::Ready) {
        _ = self.0.http.set(Arc::clone(&ctx.http));
        let commands = Command::get_global_commands(&ctx.http).await.unwrap();
        for cmd in commands {
            if cmd.name == "build_playlist" {
                Command::delete_global_command(&ctx.http, cmd.id)
                    .await
                    .unwrap();
            }
        }
        self.0.self_id.set(data_about_bot.user.id).unwrap();
        eprintln!("{} is running!", &data_about_bot.user.name);
        for runner in self.0.commands.read().await.0.values() {
            if let Some(guild) = runner.guild() {
                guild
                    .create_command(&ctx.http, runner.register())
                    .await
                    .unwrap();
            } else {
                Command::create_global_command(&ctx.http, runner.register())
                    .await
                    .unwrap();
            }
        }
        forms::check_forms(&self.0, &ctx).await.unwrap();
    }

    async fn message(&self, ctx: Context, new_message: Message) {
        if new_message.author.id.get() == 513626599330152458 {
            let mut hasher = DefaultHasher::new();
            hasher.write_u64(new_message.id.get());
            let val = hasher.finish();
            if val % 150 == 0 {
                new_message.react(&ctx.http, '🖕').await.unwrap();
            } else if val % 301 == 0 {
                new_message.react(&ctx.http, '👍').await.unwrap();
            }
        }

        let spotify = self.0.module::<SpotifyOAuth>()
            .expect("Could not find spotify module");
        self.0.module::<lp::LP>().expect("LP module not found")
            .handle_message(&spotify.client, &new_message).await;
    }

    async fn presence_update(&self, _: Context, presence: Presence) {
        if let Ok(spt_act) = self.0.module::<SpotifyActivity>() {
            spt_act.presence_update(&presence).await
        }
    }

    async fn interaction_create(&self, ctx: Context, interaction: Interaction) {
        self.0.process_interaction(ctx, interaction).await;
    }

    async fn reaction_add(&self, ctx: Context, add_reaction: serenity::model::prelude::Reaction) {
        if add_reaction.user_id == self.0.self_id.get().copied() {
            return;
        }
        ModPoll::handle_ready_poll(&self.0, &ctx, &add_reaction)
            .await
            .unwrap();
        _ = spotify::handle_reaction(&self.0, &ctx.http, &add_reaction).await;
    }

    async fn reaction_remove(
        &self,
        ctx: Context,
        remove_reaction: serenity::model::prelude::Reaction,
    ) {
        ModPoll::handle_remove_react(&self.0, &ctx, &remove_reaction)
            .await
            .unwrap()
    }

    async fn channel_pins_update(&self, ctx: Context, pin: ChannelPinsUpdateEvent) {
        let guild_id = match pin.guild_id {
            Some(gid) => gid,
            None => return,
        };
        if let Err(e) =
            Pinboard::move_pin_to_pinboard(&self.0, &ctx, pin.channel_id, guild_id).await
        {
            let guild_name = guild_id
                .name(&ctx.cache)
                .map(|name| format!("[{name}] "))
                .unwrap_or_default();
            eprintln!("{guild_name}Error moving message to pinboard: {e:?}");
        }
    }
}

async fn build_handler() -> anyhow::Result<Handler> {

    let lp = lp::LP::new();

    let conn = Connection::open("humble_ledger.sqlite")?;
    let polls = ModPoll::new("✅", "❎", "▶️", None, "<a:crabrave:996854529742094417>", lp.clone() );
    let spotify_oauth = SpotifyOAuth::new_auth_code(scopes!(
        "playlist-modify-public",
        "playlist-read-private",
        "playlist-read-collaborative",
        "user-library-read",
        "user-read-private",
        "playlist-modify-private"
    ))
    .await
    .context("spotify client")?;

    Ok(Handler::builder(conn)
        .module::<Forms>()
        .await
        .context("forms module")?
        .with_module(polls)
        .await
        .context("polls module")?
        .with_module(spotify_oauth)
        .await
        .context("spotify module")?
        .module::<AcquiringTaste>()
        .await
        .context("att module")?
        .module::<SpotifyActivity>()
        .await
        .context("spotify activity module")?
        .module::<Pinboard>()
        .await
        .context("pinboard module")?
        .module::<ModLp>()
        .await
        .context("lp module")?
        .default_command_handler(Forms::process_form_command)
        .with_module(lp)
        .await
        .context("LP module")?
        .build())
}

#[tokio::main]
async fn main() {
    let handler = build_handler().await.unwrap();

    let token = env::var("DISCORD_TOKEN").expect("Expected a token in the environment");

    let application_id: u64 = env::var("APPLICATION_ID")
        .expect("Expected an application id in the environment")
        .parse()
        .expect("application id is not a valid id");

    // Build our client.
    let mut client = serenity::Client::builder(
        token,
        GatewayIntents::GUILD_MESSAGES
            | GatewayIntents::GUILD_MESSAGE_REACTIONS
            | GatewayIntents::GUILD_PRESENCES
            | GatewayIntents::MESSAGE_CONTENT
            | GatewayIntents::GUILDS,
    )
    .event_handler(HandlerWrapper(handler))
    .application_id(ApplicationId::new(application_id))
    .await
    .expect("Error creating client");

    // Start a single shard, and start listening to events.
    //
    // Shards will automatically attempt to reconnect, and will perform
    // exponential backoff until it reconnects.
    if let Err(why) = client.start().await {
        println!("Client error: {:?}", why);
    }
}
