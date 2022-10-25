use std::time::Duration;

use anyhow::{anyhow, bail, Context as _};
use chrono::Local;
use google_sheets4::api::ValueRange;
use rspotify::prelude::Id;
use serenity::{
    async_trait,
    builder::CreateEmbed,
    client::Context,
    http::Http,
    model::prelude::{
        command::CommandOptionType,
        interaction::application_command::ApplicationCommandInteraction, CommandId, GuildId,
    },
    model::{prelude::command, Permissions},
    prelude::TypeMapKey,
};

use crate::{album_club::SubmitAlbum, spotify, Handler};
use regex::Regex;
use serenity_command::{BotCommand, CommandBuilder, CommandResponse};
use serenity_command_derive::Command;

const SUBMISSIONS_RANGE: &str = "A:F";

#[derive(Command)]
#[cmd(name = "register_playlist", desc = "Register a playlist")]
pub struct RegisterPlaylist {
    #[cmd(desc = "The name of the playlist event. The submission command will use this name.")]
    pub name: String,
    #[cmd(desc = "The identifier of the submissions google sheet.")]
    pub spreadsheet_id: String,
    #[cmd(desc = "Does this playlist allow backup picks?")]
    pub has_backup: bool,
}

pub type Playlist = RegisterPlaylist;

#[async_trait]
impl BotCommand for RegisterPlaylist {
    type Data = Handler;
    const PERMISSIONS: Permissions = Permissions::MANAGE_EVENTS;

    async fn run(
        mut self,
        handler: &Handler,
        ctx: &Context,
        interaction: &ApplicationCommandInteraction,
    ) -> anyhow::Result<serenity_command::CommandResponse> {
        let guild_id = interaction
            .guild_id
            .ok_or_else(|| anyhow!("Must be run in a guild!"))?
            .0;
        let spreadsheet_url_re =
            Regex::new(r#"https://docs.google.com/spreadsheets/d/([^/]+)"#).unwrap();
        if let Some(cap) = spreadsheet_url_re.captures(&self.spreadsheet_id) {
            self.spreadsheet_id = cap.get(1).unwrap().as_str().to_string();
        }
        handler.save_playlist(guild_id, &self).await?;
        let command_id = self.register(guild_id, &ctx.http).await?;
        let command_embed = format!("</{}:{}>", self.command_name(), command_id.0);
        Ok(CommandResponse::Public(format!(
            "Registered playlist '{}'\nUsers can add submissions with {command_embed} (`{command_embed}`)",
            &self.name,
        )))
    }
}

#[derive(Command)]
#[cmd(
    name = "remove_playlist",
    desc = "Remove a playlist submission command"
)]
pub struct RemovePlaylist {
    #[cmd(desc = "The name of the submission command", autocomplete)]
    command_name: String,
}

#[async_trait]
impl BotCommand for RemovePlaylist {
    type Data = Handler;
    const PERMISSIONS: Permissions = Permissions::MANAGE_EVENTS;

    async fn run(
        self,
        handler: &Handler,
        ctx: &Context,
        interaction: &ApplicationCommandInteraction,
    ) -> anyhow::Result<serenity_command::CommandResponse> {
        let guild_id = interaction
            .guild_id
            .ok_or_else(|| anyhow!("Must be run in a guild!"))?;
        for command in guild_id.get_application_commands(&ctx.http).await? {
            if command.name == self.command_name {
                guild_id
                    .delete_application_command(&ctx.http, command.id)
                    .await?;
                break;
            }
        }
        handler
            .delete_playlist(guild_id.0, &self.command_name)
            .await?;
        Ok(CommandResponse::Public(format!(
            "Removed command /{}",
            &self.command_name
        )))
    }
}

#[derive(Command)]
#[cmd(name = "list_playlists", desc = "List registered playlists")]
pub struct ListPlaylists {}

#[async_trait]
impl BotCommand for ListPlaylists {
    type Data = Handler;

    async fn run(
        self,
        handler: &Handler,
        ctx: &Context,
        interaction: &ApplicationCommandInteraction,
    ) -> anyhow::Result<serenity_command::CommandResponse> {
        let album_club_line = command::Command::get_global_application_commands(&ctx.http)
            .await?
            .iter()
            .find(|cmd| cmd.name == SubmitAlbum::NAME)
            .map(|cmd| format!("· Album Club: </{}:{}>", SubmitAlbum::NAME, cmd.id.0));
        let guild_id = interaction
            .guild_id
            .ok_or_else(|| anyhow!("Must be run in a guild"))?;
        let commands = guild_id.get_application_commands(&ctx.http).await?;
        let playlists = handler.list_playlists(guild_id.0).await?;
        let playlist_lines = playlists
            .into_iter()
            .filter_map(|playlist| {
                commands
                    .iter()
                    .find(|cmd| cmd.name == playlist.command_name())
                    .map(|cmd| (cmd.id.0, playlist))
            })
            .map(|(command_id, playlist)| {
                format!(
                    "· {}: </{}:{}>",
                    &playlist.name,
                    playlist.command_name(),
                    command_id
                )
            });
        let contents = album_club_line
            .into_iter()
            .chain(playlist_lines)
            .collect::<Vec<_>>()
            .join("\n");
        let mut embed = CreateEmbed::default();
        embed.title("Registered playlists");
        embed.description(contents);
        Ok(CommandResponse::Embed(embed))
    }
}

impl Playlist {
    pub fn command_name(&self) -> String {
        let slug_chars = self
            .name
            .chars()
            .filter(|c| c.is_ascii())
            .map(|c| {
                if c.is_whitespace() {
                    '_'
                } else {
                    c.to_ascii_lowercase()
                }
            })
            .filter(|&c| c.is_alphanumeric() || c == '_');
        "submit_".chars().chain(slug_chars).collect()
    }

    pub async fn register(&self, guild_id: u64, http: &Http) -> anyhow::Result<CommandId> {
        let id = GuildId(guild_id)
            .create_application_command(http, |command| {
                command
                    .name(&self.command_name())
                    .description(&format!("Submit a song to the {} playlist", &self.name))
                    .create_option(|opt| {
                        opt.name("link")
                            .description("Spotify link to your pick")
                            .kind(CommandOptionType::String)
                            .set_autocomplete(true)
                            .required(true)
                    });
                if self.has_backup {
                    command.create_option(|opt| {
                        opt.name("backup_link")
                            .description("Spotify link to your backup pick")
                            .kind(CommandOptionType::String)
                            .set_autocomplete(true)
                            .required(true)
                    });
                }
                command
            })
            .await?
            .id;
        Ok(id)
    }
}

impl TypeMapKey for Playlist {
    type Value = Self;
}

#[derive(Command)]
#[cmd(name = "", desc = "Submit a song to a playlist")]
pub struct SubmitPlaylist {
    #[cmd(desc = "Spotify link to the song", autocomplete)]
    link: String,
    #[cmd(desc = "Spotify link to the backup song", autocomplete)]
    backup_link: Option<String>,
}

#[async_trait]
impl BotCommand for SubmitPlaylist {
    type Data = Handler;

    async fn run(
        self,
        handler: &Handler,
        ctx: &Context,
        interaction: &ApplicationCommandInteraction,
    ) -> anyhow::Result<serenity_command::CommandResponse> {
        let ctx_data = ctx.data.read().await;
        let playlist = ctx_data
            .get::<Playlist>()
            .ok_or_else(|| anyhow!("Playlist not found"))?;
        let user = &interaction.user;
        let user_handle = format!("{}#{:04}", &user.name, user.discriminator);
        let mut values = vec!["".to_string(); 6];
        let now = Local::now();
        let timestamp = format!("{}", now.format("%m/%d/%Y %H:%M:%S"));
        let song = handler.spotify.get_song_from_url(&self.link).await?;
        if song.duration > Duration::from_secs(60 * 20) {
            bail!("This song is too long!")
        }
        let song_info = format!(
            "{} - {}",
            spotify::artists_to_string(&song.artists),
            &song.name
        );
        let url = song.id.unwrap().url();
        values[0] = timestamp;
        values[1] = user_handle;
        values[2].push_str(&song_info);
        values[3].push_str(&url);
        let mut backup = None;
        if let Some(link) = self.backup_link {
            let song = handler.spotify.get_song_from_url(&link).await?;
            if song.duration > Duration::from_secs(60 * 20) {
                bail!("This song is too long!")
            }
            let song_info = format!(
                "{} - {}",
                spotify::artists_to_string(&song.artists),
                &song.name,
            );
            let url = song.id.unwrap().url();
            values[4].push_str(&song_info);
            values[5].push_str(&url);
            backup = Some((song_info, url));
        }
        eprintln!("Values: {:?}", &values);

        let request = ValueRange {
            major_dimension: None,
            range: Some(SUBMISSIONS_RANGE.to_string()),
            values: Some(vec![values]),
        };
        handler
            .sheets_client
            .spreadsheets()
            .values_append(request, &playlist.spreadsheet_id, SUBMISSIONS_RANGE)
            .value_input_option("USER_ENTERED")
            .doit()
            .await
            .context("Error appending to google sheet")?;
        let resp = if let Some((backup_info, backup_url)) = backup {
            format!(
                "Submitted {} and {} to playlist\n{}\n{}",
                song_info, backup_info, url, backup_url
            )
        } else {
            format!("Submitted {} to playlist\n{}", song_info, url)
        };
        Ok(CommandResponse::Private(resp))
    }
}
