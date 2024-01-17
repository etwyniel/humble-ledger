use std::borrow::Borrow;

use anyhow::anyhow;
use serenity::all::CommandInteraction;
use serenity::builder::{CreateAutocompleteResponse, CreateInteractionResponse};
use serenity::model::prelude::UserId;

use rspotify::clients::BaseClient;
use serenity::prelude::Context;
use serenity_command::CommandBuilder;
use serenity_command_handler::album::AlbumProvider;
use serenity_command_handler::command_context::{get_focused_option, get_str_opt_ac};
use serenity_command_handler::modules::Spotify;
use serenity_command_handler::prelude::*;

use crate::forms::{DeleteFormCommand, Forms, GetSubmissions, RefreshFormCommand};
use crate::spotify_activity::SpotifyActivity;
use crate::CompletionType;

async fn get_now_playing(
    handler: &Handler,
    user_id: UserId,
) -> anyhow::Result<Option<(String, String)>> {
    let spotify: &Spotify = handler.module()?;
    let activity: &SpotifyActivity = handler.module()?;
    let Some(np) = activity.user_now_playing(user_id).await else {
        return Ok(None);
    };
    let track = spotify.client.track(np.clone()).await?;
    let name = format!(
        "{} - {}",
        Spotify::artists_to_string(&track.artists),
        &track.name
    );
    let url = format!(
        "https://open.spotify.com/track/{}",
        Borrow::<str>::borrow(&np)
    );
    Ok(Some((name, url)))
}

async fn autocomplete_link(
    handler: &Handler,
    user_id: UserId,
    option: &str,
    ty: CompletionType,
) -> Vec<(String, String)> {
    let spotify: &Spotify = handler.module().unwrap();
    if option.is_empty() && ty == CompletionType::Songs {
        match get_now_playing(handler, user_id).await {
            Ok(np) => return np.into_iter().collect(),
            Err(e) => {
                eprintln!("Error getting user's current track: {e}")
            }
        }
    }
    if option.len() >= 5 && !(option.starts_with("https://") || option.starts_with("http://")) {
        match ty {
            CompletionType::Albums => spotify.query_albums(option).await,
            CompletionType::Songs => spotify.query_songs(option).await,
        }
        .unwrap_or_default()
    } else {
        Vec::new()
    }
}

pub async fn process_autocomplete(
    handler: &Handler,
    ctx: &Context,
    ac: &CommandInteraction,
) -> anyhow::Result<bool> {
    let guild_id = ac
        .guild_id
        .ok_or_else(|| anyhow!("Must be run in a server"))?
        .get();
    let choices: Vec<_>;
    let options = &ac.data.options;
    let forms: &Forms = handler.module()?;
    let cmd_name = ac.data.name.as_str();
    match cmd_name {
        DeleteFormCommand::NAME | RefreshFormCommand::NAME | GetSubmissions::NAME => {
            let opt = get_str_opt_ac(options, "command_name").unwrap_or_default();
            choices = forms
                .forms
                .read()
                .await
                .iter()
                .filter(|form| form.guild_id == guild_id && form.command_name.contains(opt))
                .map(|form| &form.command_name)
                .map(|cmd_name| (cmd_name.clone(), cmd_name.clone()))
                .collect();
        }
        _ => {
            let forms = forms.forms.read().await;
            let form = forms
                .iter()
                .find(|form| form.guild_id == guild_id && form.command_name == cmd_name);
            if let Some(form) = form {
                let focused = match get_focused_option(options) {
                    Some(opt) => opt,
                    None => return Ok(true),
                };
                if focused.contains("spotify") || focused.contains("link") {
                    let val = match get_str_opt_ac(options, focused) {
                        Some(val) => val,
                        None => return Ok(true),
                    };
                    let ty = match form.submission_type.as_str() {
                        "album" => CompletionType::Albums,
                        _ => CompletionType::Songs,
                    };
                    choices = autocomplete_link(handler, ac.user.id, val, ty).await;
                } else {
                    return Ok(true);
                }
            } else {
                return Ok(false);
            }
        }
    }
    let resp =
        choices
            .into_iter()
            .fold(CreateAutocompleteResponse::new(), |resp, (name, value)| {
                let len = 100.min(name.len());
                resp.add_string_choice(&name[..len], value)
            });
    ac.create_response(&ctx.http, CreateInteractionResponse::Autocomplete(resp))
        .await?;
    Ok(true)
}
