use std::{fmt::Write, ops::Not, sync::Arc};

use anyhow::{anyhow, bail, Context as _};
use chrono::Utc;
use google_sheets4::api::ValueRange;
use rand::{seq::SliceRandom, thread_rng};
use reqwest::{redirect::Policy, Url};
use rspotify::{
    model::{Id, PlaylistId, TrackId, UserId},
    prelude::{BaseClient, OAuthClient, PlayableId},
};
use serenity::{
    async_trait,
    builder::{CreateInteractionResponse, EditInteractionResponse},
    client::Context,
    model::{application::CommandInteraction, Permissions},
};
use tokio::task::JoinSet;

use crate::forms::Forms;
use serenity_command::{BotCommand, CommandResponse};
use serenity_command_derive::Command;
use serenity_command_handler::{
    modules::{AlbumLookup, SpotifyOAuth},
    prelude::*,
};

const FORM_SPREADSHEET: &str = "1Hxm4SiZF7NWLvVIkK2RrnGIwFEZaoC1vR6_zl5e2TcI";
const USER_ID: &str = "cq21khhkkhy88fo9nsthvqkft";
// const GUILD_ID: GuildId = GuildId::new(400572085300101120);
// const HIGH_TASTE: RoleId = RoleId::new(427894012238757908);

#[derive(Clone, Debug)]
pub struct AcquiringTastePick {
    pub submitter: String,
    pub song: String,
    pub link: String,
}

#[derive(Clone, Debug)]
struct Variables {
    last_row: usize,
    edition: usize,
    last_playlist: Option<String>,
    current_row: usize,
}

impl Variables {
    async fn get(handler: &Handler) -> anyhow::Result<Self> {
        let forms: &Forms = handler.module()?;
        let sheets = forms.sheets_client.spreadsheets();
        let mut var_rows = sheets
            .values_get(FORM_SPREADSHEET, "Variables!A2:D2")
            .doit()
            .await?
            .1;
        let row = var_rows
            .values
            .take()
            .and_then(|mut rows| rows.pop())
            .unwrap_or_default();
        let last_row = row.get(0).and_then(|val| val.parse().ok()).unwrap_or(1);
        let edition = row
            .get(1)
            .and_then(|val| val.parse().ok())
            .unwrap_or_default();
        let last_playlist = row
            .get(2)
            .cloned()
            .and_then(|val| val.is_empty().not().then_some(val));
        let current_row = row.get(3).and_then(|val| val.parse().ok()).unwrap_or(1);
        Ok(Variables {
            last_row,
            edition,
            last_playlist,
            current_row,
        })
    }

    async fn set(self, handler: &Handler) -> anyhow::Result<()> {
        let forms: &Forms = handler.module()?;
        let sheets = forms.sheets_client.spreadsheets();
        let values = Some(vec![vec![
            self.last_row.to_string(),
            self.edition.to_string(),
            self.last_playlist.unwrap_or_default(),
        ]]);
        let req = ValueRange {
            values,
            ..Default::default()
        };
        sheets
            .values_update(req, FORM_SPREADSHEET, "Variables!A2:C2")
            .value_input_option("USER_ENTERED")
            .doit()
            .await?;
        Ok(())
    }
}

async fn pick_from_track_id(
    spotify: Arc<SpotifyOAuth>,
    submitter: &str,
    id: &str,
) -> anyhow::Result<AcquiringTastePick> {
    let track = spotify.get_song_from_id(id).await?;
    let artists = SpotifyOAuth::artists_to_string(&track.artists);
    let title = &track.name;
    Ok(AcquiringTastePick {
        submitter: submitter.to_string(),
        song: format!("{artists} - {title}"),
        link: track.id.unwrap().url(),
    })
}

async fn pick_from_shortened_link(
    spotify: Arc<SpotifyOAuth>,
    submitter: &str,
    url: &str,
) -> anyhow::Result<AcquiringTastePick> {
    let client = reqwest::Client::builder()
        .redirect(Policy::none())
        .build()
        .unwrap();
    let resp = client
        .head(url)
        .send()
        .await
        .context("Failed to resolve shortened spotify URL")?;
    let location = resp
        .headers()
        .get("location")
        .and_then(|val| val.to_str().ok())
        .ok_or_else(|| anyhow!("Not a valid spotify URL"))?;
    let url = Url::parse(location).context("Spotify shortened URL points to invalid URL")?;
    if let Some(id) = url.path().strip_prefix("/track/") {
        pick_from_track_id(spotify, submitter, id).await
    } else {
        Err(anyhow!("Not a spotify track URL: {url}"))
    }
}

async fn resolve_pick(
    spotify: Arc<SpotifyOAuth>,
    pick: AcquiringTastePick,
) -> Result<AcquiringTastePick, (AcquiringTastePick, anyhow::Error)> {
    let url = Url::parse(&pick.link)
        .context("Not a valid URL")
        .map_err(|e| (pick.clone(), e))?;
    let segments = url
        .path_segments()
        .into_iter()
        .flatten()
        .take(2)
        .collect::<Vec<_>>();
    match (url.domain(), segments.as_slice()) {
        (Some("open.spotify.com"), ["track", id]) => {
            pick_from_track_id(spotify, &pick.submitter, id).await
        }
        (Some("spotify.link"), [_]) => {
            eprintln!("Found shortened link, resolving it");
            pick_from_shortened_link(spotify, &pick.submitter, &pick.link).await
        }
        _ => return Err((pick, anyhow!("Not a spotify URL"))),
    }
    .map_err(|e| (pick, e))
}

async fn build_playlist<'a, 'b: 'a>(
    handler: &'a Handler,
    picks: &'b [AcquiringTastePick],
    playlist: Option<PlaylistId<'static>>,
    edition: usize,
) -> anyhow::Result<(
    PlaylistId<'static>,
    Vec<AcquiringTastePick>,
    Vec<(AcquiringTastePick, String)>,
)> {
    let spotify: Arc<SpotifyOAuth> = handler.module_arc()?;
    spotify.client.refresh_token().await?;
    let user_id: UserId<'static> = UserId::from_id(USER_ID)?;
    let playlist = match playlist {
        None => {
            let date = Utc::now().date_naive().format("%Y-%m-%d");
            let resp = spotify
                .client
                .user_playlist_create(
                    user_id,
                    &format!("I&W Acquiring the Taste #{edition} | {date}"),
                    Some(true),
                    None,
                    None,
                )
                .await
                .context("failed to create playlist")?;
            resp.id
        }
        Some(id) => id,
    };
    let mut invalid = Vec::new();
    let mut valid = Vec::new();
    let spotify: Arc<SpotifyOAuth> = handler.module_arc()?;
    let mut set = JoinSet::new();
    for pick in picks {
        set.spawn(resolve_pick(Arc::clone(&spotify), pick.clone()));
    }
    let mut picks_resolved = Vec::with_capacity(picks.len());
    while let Some(res) = set.join_next().await {
        match res.unwrap() {
            Ok(pick) => picks_resolved.push(pick),
            Err((pick, e)) => invalid.push((pick, e.to_string())),
        }
    }
    let items = picks_resolved
        .iter()
        .flat_map(|pick| {
            let Ok(url) = Url::parse(&pick.link) else {
                invalid.push((pick.clone(), format!("not a url: {}", &pick.link)));
                return None;
            };
            let Some(id) = url.path().strip_prefix("/track/") else {
                invalid.push((
                    pick.clone(),
                    format!("not a spotify track url: <{}>", &pick.link),
                ));
                return None;
            };
            match TrackId::from_id_or_uri(id) {
                Ok(id) => {
                    valid.push(pick.clone());
                    Some(id.clone_static())
                }
                Err(e) => {
                    invalid.push((pick.clone(), e.to_string()));
                    None
                }
            }
        })
        .map(PlayableId::from);
    let items: Vec<_> = items.collect();
    spotify
        .client
        .playlist_add_items(playlist.as_ref(), items, None)
        .await
        .context("failed to add songs to playlist")?;
    Ok((playlist, valid, invalid))
}

// gets new submissions from the form and stores them in the database
async fn get_acquiring_taste_submissions(
    handler: &Handler,
) -> anyhow::Result<Vec<AcquiringTastePick>> {
    let forms: &Forms = handler.module()?;
    let sheets = forms.sheets_client.spreadsheets();
    let rows = sheets
        .values_get(FORM_SPREADSHEET, "Deduplicated!A:C")
        .doit()
        .await
        .context("failed to get submissions")?
        .1;
    let Some(values) = rows.values else {
        bail!("No submissions found on this sheet");
    };
    let picks = values
        .into_iter()
        .map(|row| AcquiringTastePick {
            submitter: row[0].clone(),
            song: row[1].clone(),
            link: row[2].clone(),
        })
        .collect();
    Ok(picks)
}

async fn build_playlist_from_picks(
    handler: &Handler,
    _ctx: &Context,
    increment_edition: bool,
) -> anyhow::Result<String> {
    let Variables {
        last_row: _,
        edition,
        last_playlist,
        current_row,
    } = Variables::get(handler).await?;
    let mut picks = get_acquiring_taste_submissions(handler).await?;
    if picks.is_empty() {
        return Ok("No new picks to add".to_string());
    }
    {
        let mut rng = thread_rng();
        picks.shuffle(&mut rng);
    }
    let playlist_id = if increment_edition {
        None
    } else {
        last_playlist.as_ref().and_then(|p| {
            PlaylistId::from_id_or_uri(p)
                .ok()
                .map(|id| id.clone_static())
        })
    };
    let edition = edition + if increment_edition { 1 } else { 0 };
    let (playlist, valid, invalid) = build_playlist(handler, &picks, playlist_id, edition).await?;
    let nvalid = valid.len();
    let variables = Variables {
        last_row: current_row,
        edition,
        last_playlist: Some(playlist.to_string()),
        current_row: 0, // not used
    };
    let sheets = handler.module::<Forms>()?.sheets_client.spreadsheets();
    let playlist_url = playlist.url();
    if increment_edition {
        let req = ValueRange {
            values: Some(vec![vec![
                variables.edition.to_string(),
                Utc::now().date_naive().format("%Y-%m-%d").to_string(),
                playlist_url.clone(),
            ]]),
            ..Default::default()
        };
        sheets
            .values_append(req, FORM_SPREADSHEET, "Playlists!A:C")
            .value_input_option("USER_ENTERED")
            .doit()
            .await
            .context("failed to add playlist to spreadsheet")?;
    }
    let mut picks_values = Vec::with_capacity(picks.len());
    for pick in valid {
        // let members = GUILD_ID
        //     .search_members(&ctx.http, &pick.submitter, Some(1))
        //     .await?;
        // if members.is_empty() || !members[0].roles.contains(&HIGH_TASTE) {
        //     invalid.push((pick, "not high taste".to_string()));
        //     continue;
        // }
        // let user_id = members
        //     .get(0)
        //     .map(|member| member.user.id.0.to_string())
        //     .unwrap_or_default();
        let user_id = String::new();
        let row = vec![
            variables.edition.to_string(),
            pick.submitter,
            user_id,
            pick.song,
            pick.link,
        ];
        picks_values.push(row);
    }
    if !picks_values.is_empty() {
        let req = ValueRange {
            values: Some(picks_values),
            ..Default::default()
        };
        sheets
            .values_append(req, FORM_SPREADSHEET, "Picks!A1:E1")
            .value_input_option("USER_ENTERED")
            .doit()
            .await
            .context("failed to save picks to spreadsheet")?;
    }
    variables
        .set(handler)
        .await
        .context("failed to save variables to spreadsheet")?;
    let mut resp = if last_playlist.is_none() || increment_edition {
        format!(
            "Created a playlist with {nvalid} tracks.\n{}",
            &playlist_url
        )
    } else {
        format!(
            "Added {nvalid} tracks to existing playlist.\n{}",
            &playlist_url
        )
    };
    if !invalid.is_empty() {
        _ = write!(
            &mut resp,
            "\n{} picks were invalid and could not be added:",
            invalid.len()
        );
        invalid.into_iter().for_each(|(pick, reason)| {
            _ = write!(
                &mut resp,
                "\n{}'s pick ({}): {}",
                pick.submitter, pick.song, reason
            );
        })
    }
    Ok(resp)
}

#[derive(Command)]
#[cmd(
    name = "build_playlist",
    desc = "Build the Acquiring the Taste playlist from user submissions"
)]
pub struct BuildPlaylist {
    reuse: Option<bool>,
}

#[async_trait]
impl BotCommand for BuildPlaylist {
    type Data = Handler;
    const PERMISSIONS: Permissions = Permissions::MANAGE_EVENTS;
    // const GUILD: Option<GuildId> = Some(GUILD_ID);
    async fn run(
        self,
        handler: &Handler,
        ctx: &Context,
        interaction: &CommandInteraction,
    ) -> anyhow::Result<CommandResponse> {
        interaction
            .create_response(
                &ctx.http,
                CreateInteractionResponse::Defer(Default::default()),
            )
            .await?;
        let res = build_playlist_from_picks(handler, ctx, !self.reuse.unwrap_or(false))
            .await
            .context("Error getting new submissions");
        let resp = match res {
            Ok(resp) => resp,
            Err(e) => {
                eprintln!("{e:?}");
                e.to_string()
            }
        };
        interaction
            .edit_response(&ctx.http, EditInteractionResponse::new().content(&resp))
            .await?;
        Ok(CommandResponse::None)
    }
}

pub struct AcquiringTaste {}

#[async_trait]
impl Module for AcquiringTaste {
    async fn add_dependencies(builder: HandlerBuilder) -> anyhow::Result<HandlerBuilder> {
        builder
            .module::<SpotifyOAuth>()
            .await?
            .module::<AlbumLookup>()
            .await
    }

    async fn init(_: &ModuleMap) -> anyhow::Result<Self> {
        Ok(AcquiringTaste {})
    }

    fn register_commands(
        &self,
        store: &mut CommandStore,
        _completion_handlers: &mut CompletionStore,
    ) {
        store.register::<BuildPlaylist>();
        // store.register::<GetMySubmissions>();
    }
}
