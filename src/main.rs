use std::fmt::Write;
use std::ops::Deref;
use std::sync::Arc;
use std::time::Instant;
use std::{collections::HashMap, env};

use album::AlbumProvider;
use anyhow::{anyhow, Context as _};
use bandcamp::Bandcamp;
use google_sheets4::Sheets;
use hyper::client::HttpConnector;
use hyper_tls::HttpsConnector;
use oauth::ServiceAccountAuthenticator;
use playlist::{Playlist, RemovePlaylist, SubmitPlaylist};
use rusqlite::Connection;
use serenity::http::Http;
use serenity::model::prelude::UnavailableGuild;
use serenity::prelude::Mutex;
use serenity::utils::Color;
use serenity::{
    async_trait,
    client::{Context, EventHandler},
    model::{
        application::command::Command,
        prelude::interaction::{
            application_command::{
                ApplicationCommandInteraction, CommandDataOption, CommandDataOptionValue,
            },
            autocomplete::AutocompleteInteraction,
            Interaction,
        },
    },
    prelude::{GatewayIntents, RwLock},
};
use serenity_command::{BotCommand, CommandBuilder, CommandResponse, CommandRunner};
use spotify::Spotify;
use youtube::Youtube;
use yup_oauth2 as oauth;

use forms::{CommandFromForm, DeleteFormCommand, FormCommand, FormsClient, ListForms};

mod album;
mod album_club;
mod bandcamp;
mod db;
mod forms;
mod playlist;
mod spotify;
mod youtube;

pub fn get_str_opt_ac<'a>(options: &'a [CommandDataOption], name: &str) -> Option<&'a str> {
    options
        .iter()
        .find(|opt| opt.name == name)
        .and_then(|opt| opt.value.as_ref())
        .and_then(|val| val.as_str())
}

pub fn get_focused_option(options: &[CommandDataOption]) -> Option<&str> {
    options
        .iter()
        .find(|opt| opt.focused)
        .map(|opt| opt.name.as_str())
}

#[derive(Eq, PartialEq)]
enum CompletionType {
    Albums,
    Songs,
}

pub struct Handler {
    sheets_client: Sheets<HttpsConnector<HttpConnector>>,
    commands: RwLock<HashMap<&'static str, Box<dyn CommandRunner<Handler> + Send + Sync>>>,
    spotify: Arc<Spotify>,
    providers: Vec<Arc<dyn AlbumProvider>>,
    db: Arc<Mutex<Connection>>,
    forms_client: FormsClient,
    forms: Arc<RwLock<Vec<FormCommand>>>,
}

impl Handler {
    async fn process_command(
        &self,
        ctx: &Context,
        interaction: &ApplicationCommandInteraction,
    ) -> anyhow::Result<CommandResponse> {
        let guild_id = interaction
            .guild_id
            .ok_or_else(|| anyhow!("Must be run in a server"))?
            .0;
        let data = &interaction.data;
        if let Some(runner) = self.commands.read().await.get(data.name.as_str()) {
            runner.run(self, ctx, interaction).await
        } else {
            let forms = self.forms.read().await;
            let form = forms
                .iter()
                .find(|form| form.guild_id == guild_id && form.command_name == data.name);
            if let Some(form) = form {
                return form
                    .form
                    .submit(self, ctx, interaction, &form.submission_type)
                    .await;
            }
            let playlist = self
                .get_playlist(guild_id, &data.name)
                .await
                .context("Unknown command")?;
            ctx.data.write().await.insert::<Playlist>(playlist);
            let data: SubmitPlaylist = data.into();
            data.run(self, ctx, interaction)
                .await
                .context("Error submitting to playlist")
        }
    }

    async fn autocomplete_link(&self, option: &str, ty: CompletionType) -> Vec<(String, String)> {
        if option.len() >= 5 && !(option.starts_with("https://") && option.starts_with("http://")) {
            match ty {
                CompletionType::Albums => self.spotify.query_albums(option).await,
                CompletionType::Songs => self.spotify.query_songs(option).await,
            }
            .unwrap_or_default()
        } else {
            Vec::new()
        }
    }

    pub async fn autocomplete_album_link(
        &self,
        options: &[CommandDataOption],
    ) -> anyhow::Result<Vec<(String, String)>> {
        let mut choices = vec![];
        let focused = get_focused_option(options);
        let link = get_str_opt_ac(options, "link");
        if let (Some(s), Some("link")) = (&link, focused) {
            choices = self.autocomplete_link(s, CompletionType::Albums).await;
        }
        Ok(choices)
    }

    pub async fn autocomplete_song_link(
        &self,
        options: &[CommandDataOption],
    ) -> anyhow::Result<Vec<(String, String)>> {
        let mut choices = vec![];
        let focused = get_focused_option(options);
        let link = get_str_opt_ac(options, "link");
        let backup_link = get_str_opt_ac(options, "backup_link");
        if let (Some(s), Some("link")) = (link, focused) {
            choices = self.autocomplete_link(s, CompletionType::Songs).await;
        } else if let (Some(s), Some("backup_link")) = (backup_link, focused) {
            choices = self.autocomplete_link(s, CompletionType::Songs).await;
        }
        Ok(choices)
    }

    async fn process_autocomplete(
        &self,
        ctx: &Context,
        ac: AutocompleteInteraction,
    ) -> anyhow::Result<()> {
        let guild_id = ac
            .guild_id
            .ok_or_else(|| anyhow!("Must be run in a server"))?
            .0;
        let choices: Vec<(String, String)>;
        let options = &ac.data.options;
        let cmd_name = ac.data.name.as_str();
        match cmd_name {
            album_club::SubmitAlbum::NAME => {
                choices = self.autocomplete_album_link(options).await?;
            }
            RemovePlaylist::NAME => {
                let ac_opt = get_str_opt_ac(options, "command_name");
                choices = self
                    .list_playlists(guild_id)
                    .await?
                    .into_iter()
                    .map(|pl| {
                        let command_name = pl.command_name();
                        (pl.name, command_name)
                    })
                    .filter(|(name, slug)| {
                        ac_opt
                            .map(|prompt| name.contains(prompt) || slug.contains(prompt))
                            .unwrap_or(true)
                    })
                    .collect();
            }
            DeleteFormCommand::NAME => {
                let opt = get_str_opt_ac(options, "command_name").unwrap_or_default();
                choices = self
                    .forms
                    .read()
                    .await
                    .iter()
                    .filter(|form| form.guild_id == guild_id && form.command_name.contains(&opt))
                    .map(|form| &form.command_name)
                    .map(|cmd_name| (cmd_name.clone(), cmd_name.clone()))
                    .collect();
            }
            name => {
                let forms = self.forms.read().await;
                let form = forms
                    .iter()
                    .find(|form| form.guild_id == guild_id && form.command_name == cmd_name);
                if let Some(form) = form {
                    let focused = match get_focused_option(options) {
                        Some(opt) => opt,
                        None => return Ok(()),
                    };
                    if focused.contains("spotify") || focused.contains("link") {
                        let val = match get_str_opt_ac(options, focused) {
                            Some(val) => val,
                            None => return Ok(()),
                        };
                        let ty = match form.submission_type.as_str() {
                            "album" => CompletionType::Albums,
                            _ => CompletionType::Songs,
                        };
                        choices = self.autocomplete_link(val, ty).await;
                    } else {
                        return Ok(());
                    }
                } else {
                    let _playlist = self.get_playlist(guild_id, name).await?;
                    choices = self.autocomplete_song_link(options).await?;
                }
            }
        }
        ac.create_autocomplete_response(&ctx.http, |r| {
            choices.into_iter().for_each(|(name, value)| {
                r.add_string_choice(name, value);
            });
            r
        })
        .await
        .map_err(anyhow::Error::from)
    }

    async fn register_playlist_commands(
        &self,
        http: &Http,
        guilds: &[UnavailableGuild],
    ) -> anyhow::Result<()> {
        for g in guilds.iter().filter(|g| !g.unavailable).map(|g| g.id.0) {
            for playlist in self.list_playlists(g).await? {
                playlist.register(g, http).await?;
            }
        }
        Ok(())
    }
}

// Format command options for debug output
fn format_options(opts: &[CommandDataOption]) -> String {
    let mut out = String::new();
    for (i, opt) in opts.iter().enumerate() {
        if i > 0 {
            out.push(' ');
        }
        out.push_str(&opt.name);
        out.push_str(": ");
        match &opt.resolved {
            None => out.push_str("None"),
            Some(CommandDataOptionValue::String(s)) => write!(&mut out, "{s:?}").unwrap(),
            Some(val) => write!(&mut out, "{val:?}").unwrap(),
        }
    }
    out
}

#[async_trait]
impl EventHandler for Handler {
    async fn ready(&self, ctx: Context, data_about_bot: serenity::model::gateway::Ready) {
        eprintln!("{} is running!", &data_about_bot.user.name);
        for runner in self.commands.read().await.values() {
            Command::create_global_application_command(&ctx.http, |command| {
                runner.register(command)
            })
            .await
            .unwrap();
        }
        forms::check_forms(self, &ctx).await.unwrap();
        self.register_playlist_commands(&ctx.http, &data_about_bot.guilds)
            .await
            .unwrap();
    }

    async fn interaction_create(&self, ctx: Context, interaction: Interaction) {
        if let Interaction::Autocomplete(ac) = interaction {
            let cmd_name = ac.data.name.clone();
            if let Err(e) = self.process_autocomplete(&ctx, ac).await {
                eprintln!(
                    "Error processing automplete interaction for /{}: {:?}",
                    cmd_name, e
                );
            }
        } else if let Interaction::ApplicationCommand(command) = interaction {
            // log command
            let guild_name = if let Some(guild_id) = command.guild_id {
                let name = guild_id.to_partial_guild(&ctx.http).await.unwrap().name;
                format!("[{name}] ")
            } else {
                String::new()
            };
            let user = &command.user.name;
            let name = &command.data.name;
            let params = format_options(&command.data.options);
            eprintln!("{guild_name}{user}: /{name} {params}");

            let start = Instant::now();
            let resp = self.process_command(&ctx, &command).await;
            let elapsed = start.elapsed();
            eprintln!("{guild_name}{user}: /{name} -({:?})-> {:?}", elapsed, &resp);
            let resp = match resp {
                Ok(resp) => resp,
                Err(e) => CommandResponse::Private(e.to_string()),
            };

            let (contents, mut embeds, flags) = match resp.to_contents_and_flags() {
                None => return,
                Some(c) => c,
            };
            if let Some(embed) = &mut embeds {
                embed.color(Color::DARK_GREEN);
            }
            if let Err(why) = command.create_interaction_response(&ctx.http, |resp|
                resp
                .kind(serenity::model::application::interaction::InteractionResponseType::ChannelMessageWithSource)
                .interaction_response_data(|message| {
                    embeds.into_iter().for_each(|em| {message.add_embed(em);});
                    message
                    .content(&contents)
                    .flags(flags)
                })
            ).await {
                eprintln!("cannot respond to slash command: {:?}", why);
                return;
            }
        }
    }
}

fn register_command<C: for<'a> CommandBuilder<'a>>(
    commands: &mut HashMap<&'static str, Box<dyn CommandRunner<Handler> + Send + Sync>>,
) where
    C: BotCommand<Data = Handler>,
{
    commands.insert(C::NAME, C::runner());
}

#[tokio::main]
async fn main() {
    // Initialize google credentials
    let conn = hyper_tls::HttpsConnector::new();
    let client = hyper::Client::builder().build(conn);
    let client_secret = oauth::read_service_account_key(&"credentials.json".to_string())
        .await
        .unwrap();
    let authenticator = ServiceAccountAuthenticator::with_client(client_secret, client.clone())
        .build()
        .await
        .unwrap();

    let mut commands = HashMap::new();
    register_command::<CommandFromForm>(&mut commands);
    register_command::<DeleteFormCommand>(&mut commands);
    register_command::<ListForms>(&mut commands);
    let commands = RwLock::new(commands);
    let spotify = Arc::new(Spotify::new().await.unwrap());
    let providers: Vec<Arc<(dyn AlbumProvider + 'static)>> = vec![
        Arc::clone(&spotify) as _,
        Arc::new(Bandcamp::new()),
        Arc::new(Youtube::new(&client, &authenticator)),
    ];
    let sheets_client = google_sheets4::api::Sheets::new(client.clone(), authenticator.clone());
    let db = Arc::new(Mutex::new(db::init().unwrap()));
    let forms_client = FormsClient {
        authenticator,
        client,
    };
    let forms = Arc::new(RwLock::new(
        forms::load_forms(db.lock().await.deref()).unwrap(),
    ));
    let handler = Handler {
        sheets_client,
        commands,
        spotify,
        providers,
        db,
        forms_client,
        forms,
    };

    let token = env::var("DISCORD_TOKEN").expect("Expected a token in the environment");

    let application_id: u64 = env::var("APPLICATION_ID")
        .expect("Expected an application id in the environment")
        .parse()
        .expect("application id is not a valid id");

    // Build our client.
    let mut client = serenity::Client::builder(token, GatewayIntents::GUILD_MESSAGES)
        .event_handler(handler)
        .application_id(application_id)
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
