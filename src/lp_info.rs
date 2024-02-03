use anyhow::Context as _;
use futures_util::stream::{StreamExt, TryStreamExt};
use itertools::Itertools;
use once_cell::sync::Lazy;
use regex::Regex;
use rspotify::clients::BaseClient;
use rspotify::model::{FullEpisode, FullTrack, PlayableItem, PlaylistItem};
use serenity::all::InteractionResponseFlags;
use serenity::builder::{
    CreateEmbed, CreateInteractionResponse, CreateInteractionResponseMessage,
};
use serenity::model::prelude::CommandInteraction;
use serenity::model::prelude::{ChannelId, Message};
use serenity::{async_trait, prelude::Context};
use serenity_command::{BotCommand, CommandResponse};
use serenity_command_derive::Command;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::RwLock;

use serenity_command_handler::events; // serenity-command-handler, for hooking

use serenity_command_handler::modules::polls::ReadyPollStarted;
use serenity_command_handler::modules::Spotify;

use serenity_command_handler::{
    CommandStore, CompletionStore, Handler, HandlerBuilder, Module, ModuleMap,
};

#[derive(Debug)]
pub struct TrackInfo {
    pub number: usize,
    pub name: String,
    pub uri: Option<String>,
    pub duration: chrono::Duration,
}

#[derive(Debug)]
enum PlaylistInfo {
    AlbumInfo {
        id: String,
        artist: String,
        name: String,
        uri: Option<String>,
    },
    PlaylistInfo {
        id: String,
        name: String,
        uri: Option<String>,
    },
}

#[derive(Debug)]
pub struct LPInfo {
    playlist: PlaylistInfo,
    tracks: Vec<TrackInfo>,
    started: Option<chrono::DateTime<chrono::Utc>>,
}

impl LPInfo {
    async fn from_spotify_album_id<C: BaseClient>(
        client: &C,
        album_id_str: &str,
    ) -> anyhow::Result<Self> {
        let album_id = rspotify::model::AlbumId::from_id(album_id_str)
            .context("trying to parse album ID")?;

        let album = client
            .album(album_id.clone(), None)
            .await
            .context("fetching album")?;
        let artists = album
            .artists
            .iter()
            .map(|a| a.name.as_ref())
            .collect::<Vec<_>>()
            .join(", ");
        eprintln!("Album pinged: {} - {} ", &artists, &album.name); // Debug
        let tracks = client
            .album_track(album_id, None)
            .enumerate()
            .map(|(count, mb)| mb.map(|x| (count, x)))
            .map_ok(|(count, track)| TrackInfo {
                number: count + 1,
                name: track.name.to_string(),
                duration: track.duration.clone(),
                uri: track.external_urls.get("spotify").map(|s| s.to_owned()),
            })
            .try_collect::<Vec<TrackInfo>>()
            .await?;

        Ok(LPInfo {
            playlist: PlaylistInfo::AlbumInfo {
                id: album.id.to_string(),
                artist: artists.clone(),
                name: album.name.to_string(),
                uri: album.external_urls.get("spotify").map(|s| s.to_owned()),
            },
            tracks,
            started: None,
        })
    }
    async fn from_spotify_playlist_id<C: BaseClient>(
        client: &C,
        album_id_str: &str,
    ) -> anyhow::Result<Self> {
        let playlist_id = rspotify::model::PlaylistId::from_id(album_id_str)
            .context("trying to parse playlist ID")?;

        let playlist = client
            .playlist(playlist_id.clone(), None, None)
            .await
            .context("fetching playlist")?;

        eprintln!("Playlist pinged: {}", &playlist.name); // Debug
        let items = client
            .playlist_items(playlist_id, None, None)
            .try_collect::<Vec<PlaylistItem>>()
            .await?;
        let tracks = items
            .iter()
            .enumerate()
            .filter_map(|(count, item)| {
                item.track.as_ref().map(|track| match track {
                    PlayableItem::Track(FullTrack {
                        name,
                        duration,
                        external_urls,
                        ..
                    })
                    | PlayableItem::Episode(FullEpisode {
                        name,
                        duration,
                        external_urls,
                        ..
                    }) => TrackInfo {
                        number: count + 1,
                        name: name.to_string(),
                        duration: duration.clone(),
                        uri: external_urls.get("spotify").map(|s| s.to_owned()),
                    },
                })
            })
            .collect::<Vec<_>>();

        Ok(LPInfo {
            playlist: PlaylistInfo::PlaylistInfo {
                id: playlist.id.to_string(),
                name: playlist.name.to_string(),
                uri: playlist
                    .external_urls
                    .get("spotify")
                    .map(|s| s.to_owned()),
            },
            tracks,
            started: None,
        })
    }

    // Find spotify album or playlist in line and fetch info
    async fn from_match_string<C: BaseClient>(
        client: &C,
        string: &str,
    ) -> anyhow::Result<Option<Self>> {
        if let Some(aid) = match_spotify_album(string) {
            return Ok(Some(Self::from_spotify_album_id(client, aid).await?));
        }
        if let Some(pid) = match_spotify_playlist(string) {
            return Ok(Some(
                Self::from_spotify_playlist_id(client, pid).await?,
            ));
        }
        return Ok(None);
    }
}

enum PlayState<'a> {
    NotStarted,
    Finished(chrono::Duration), // how long ago
    Playing {
        track: &'a TrackInfo,
        position: chrono::Duration,
    },
}

// Turn a string into a link if an URI is available.
fn maybe_uri<S: AsRef<str>, T: AsRef<str>>(
    text: T,
    mb_uri: Option<S>,
) -> String {
    match mb_uri.as_ref() {
        None => text.as_ref().to_string(),
        Some(uri) => format!("[{}]({})", text.as_ref(), uri.as_ref()),
    }
}

impl LPInfo {
    // Calculate what's playing after `offset`
    fn now_playing(&self, offset: chrono::Duration) -> PlayState {
        let started = match self.started {
            None => {
                return PlayState::NotStarted;
            }
            Some(started) => started,
        };
        let now = chrono::offset::Utc::now();
        if started > now {
            eprintln!(
                "LPInfo: Started timestamp in the future! started={} > now={}",
                started, now
            );
            return PlayState::NotStarted;
        }
        let mut remain = now - started + offset;
        for track in self.tracks.iter() {
            if track.duration > remain {
                return PlayState::Playing {
                    track: &track,
                    position: remain,
                };
            } else {
                remain = remain - track.duration;
            }
        }
        // We passed all the tracks
        // remain = now - started - sum(track duration)
        // How long ago the playlist finished
        PlayState::Finished(remain)
    }

    // Build discord embed for lp_info
    fn build_info_embed(&self) -> CreateEmbed {
        let (lp_name, lp_id) = match &self.playlist {
            PlaylistInfo::AlbumInfo {
                id,
                artist,
                name,
                uri,
            } => {
                let album_name =
                    maybe_uri(format!("{artist} - {name}"), uri.as_ref());
                (format!("**Album**:\n {album_name}"), id.clone())
            }
            PlaylistInfo::PlaylistInfo { id, name, uri } => {
                let playlist_name = maybe_uri(name, uri.as_ref());
                (format!("**Playlist**:\n {playlist_name}"), id.clone())
            }
        };

        let mut embed = CreateEmbed::new().description(format!(
            "{} - \\[{}\\]",
            lp_name,
            display_duration(&self.tracks.iter().map(|t| t.duration).sum())
        ));
        match self.now_playing(chrono::Duration::seconds(0)) {
            PlayState::NotStarted => {
                embed = embed.title("Listening Party has not started yet.");
            }
            PlayState::Finished(_) => {
                embed = embed.title("Listening Party has finished.");
            }
            PlayState::Playing {
                track, position, ..
            } => {
                let now = chrono::offset::Utc::now();
                let track_uri_ctx = track
                    .uri
                    .as_ref()
                    .map(|uri| format!("{}?context={}", uri, &lp_id));
                embed = embed
                    .title("Listening Party in full swing! Join in!")
                    .field(
                        "Now playing",
                        format!(
                            "Track {} - {} - ({})\nTrack started <t:{}:R>",
                            track.number,
                            maybe_uri(&track.name, track_uri_ctx.as_ref()),
                            display_duration(&track.duration),
                            (now - position).timestamp(),
                        ),
                        true,
                    );
            }
        }
        embed
    }

    // Build discord embed for lp_join
    fn build_join_embed(&self, offset: chrono::Duration) -> CreateEmbed {
        let lp_id = match &self.playlist {
            PlaylistInfo::AlbumInfo { id, .. }
            | PlaylistInfo::PlaylistInfo { id, .. } => id.clone(),
        };
        let mut embed = CreateEmbed::new();
        match self.now_playing(offset) {
            PlayState::NotStarted => {
                embed = embed.title("Listening Party has not started yet.");
            }
            PlayState::Finished(_) => {
                embed = embed.title("Listening Party has finished.");
            }
            PlayState::Playing { track, position } => {
                let now = chrono::offset::Utc::now();
                let track_uri_ctx = track
                    .uri
                    .as_ref()
                    .map(|uri| format!("{}?context={}", uri, &lp_id));
                embed = embed.title("Join this listening party").field(
                    "Select song",
                    format!(
                        "Track: {} - ({})\nPosition **{}**\n Start playback: <t:{}:R>",
                        maybe_uri(&track.name, track_uri_ctx.as_ref()),
                        display_duration(&track.duration),
                        display_duration(&position),
                        (now + offset).timestamp()
                    ),
                    true,
                );
            }
        }
        embed
    }
}

// Format Duration as hh:mm:ss
fn display_duration(duration: &chrono::Duration) -> String {
    let allsecs = duration.num_seconds();
    let seconds = allsecs % 60;
    let minutes = allsecs / 60 % 60;
    let hours = allsecs / 3600;
    if hours > 0 {
        format!("{}:{:0>2}:{:0>2}", hours, minutes, seconds)
    } else {
        format!("{:0>2}:{:0>2}", minutes, seconds)
    }
}

// Regex to identity spotify album URIs and extract album id
static SPOTIFY_ALBUM_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(
        "\\bhttps://open.spotify.com(?:/intl-[a-z]+)?\
            /album/([a-zA-Z0-9]+)(?:\\?[a-zA-Z?=&]*)?\\b",
    )
    .unwrap()
});

// Find spotify playlist URI and extract the album ID
fn match_spotify_album(string: &str) -> Option<&str> {
    SPOTIFY_ALBUM_RE
        .captures(string.as_ref())
        .map(|caps| caps.get(1).unwrap().as_str())
}

// Regex to identity spotify playlist URIs and extract album id
const SPOTIFY_PLAYLIST_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(
        "\\bhttps://open.spotify.com(?:/intl-[a-z]+)?\
           /playlist/([a-zA-Z0-9]+)(?:\\?[a-zA-Z?=&]*)?\\b",
    )
    .unwrap()
});

// Find spotify playlist URI and extract the album ID
fn match_spotify_playlist(string: &str) -> Option<&str> {
    SPOTIFY_PLAYLIST_RE
        .captures(string.as_ref())
        .map(|caps| caps.get(1).unwrap().as_str())
}

#[derive(Command, Debug)]
#[cmd(name = "lp_info", desc = "Check if listening party is going")]
pub struct CurrentLP {
    #[cmd(desc = "Should the answer be visible to everyone?")]
    visible: Option<bool>,
}

#[async_trait]
impl BotCommand for CurrentLP {
    type Data = Handler;
    async fn run(
        self,
        data: &Handler,
        ctx: &Context,
        interaction: &CommandInteraction,
    ) -> anyhow::Result<CommandResponse> {
        let mut msg = {
            // Find last LP
            let lps = data.module::<LP>().unwrap().last_pinged.read().unwrap();
            let lp = lps.get(&interaction.channel_id);
            match lp {
                None => CreateInteractionResponseMessage::new()
                    .content("There is no listening party at the moment."),
                Some(lpinfo) => CreateInteractionResponseMessage::new()
                    .add_embed(lpinfo.build_info_embed()),
            }
        };

        if !self.visible.unwrap_or(false) {
            msg = msg.flags(InteractionResponseFlags::EPHEMERAL);
        }
        interaction
            .create_response(&ctx.http, CreateInteractionResponse::Message(msg))
            .await?;
        Ok(CommandResponse::None)
    }
}

#[derive(Command, Debug)]
#[cmd(name = "lp_join", desc = "Join a listening party (privately)")]
pub struct JoinLP {
    #[cmd(desc = "Seconds to start playing")]
    offset: Option<u64>,
}

#[async_trait]
impl BotCommand for JoinLP {
    type Data = Handler;
    async fn run(
        self,
        data: &Handler,
        ctx: &Context,
        interaction: &CommandInteraction,
    ) -> anyhow::Result<CommandResponse> {
        let offset =
        // This can overflow, but we don't really care
        // garbage in, garbage out
            chrono::Duration::seconds(self.offset.unwrap_or(15) as i64);
        let msg = {
            // Find last LP
            let lps = data.module::<LP>().unwrap().last_pinged.read().unwrap();
            let lp = lps.get(&interaction.channel_id);
            match lp {
                None => CreateInteractionResponseMessage::new()
                    .content("There is no listening party at the moment."),
                Some(lpinfo) => CreateInteractionResponseMessage::new()
                    .add_embed(lpinfo.build_join_embed(offset)),
            }
        }
        .flags(InteractionResponseFlags::EPHEMERAL);

        interaction
            .create_response(&ctx.http, CreateInteractionResponse::Message(msg))
            .await?;
        Ok(CommandResponse::None)
    }
}

pub type PingedMap = Arc<RwLock<HashMap<ChannelId, LPInfo>>>;

pub struct LP {
    last_pinged: PingedMap,
}

impl Clone for LP {
    fn clone(&self) -> Self {
        LP {
            last_pinged: Arc::clone(&self.last_pinged),
        }
    }
}

// Roles used for pinging listening parties
const LP_ROLES: &'static [&'static str] =
    &[&"Listening Party", &"Impromptu Listening Party"];

impl LP {
    pub fn new() -> Self {
        LP {
            last_pinged: Default::default(),
        }
    }

    // Handle messages to remember the last pinged album
    pub async fn handle_message<C: BaseClient>(
        &self,
        client: &C,
        ctx: &Context,
        msg: &Message,
    ) {
        let msg_txt: &str = &msg.content;

        // Check if the specified roles were mentioned
        if msg
            .mention_roles
            .iter()
            // Resolve ID to role
            .filter_map(|rid| {
                rid.to_role_cached(&ctx.cache).or_else(|| {
                    eprintln!("Role {rid} not found");
                    None
                })
            })
            .any(|role| LP_ROLES.contains(&role.name.as_ref()))
        {
            let pl = match LPInfo::from_match_string(client, msg_txt).await {
                Err(e) => {
                    eprintln!("Error resolving spotify link: {}", e);
                    return;
                }
                Ok(Some(pl)) => pl,
                Ok(None) => return,
            };
            // Store album/playlist in channel info
            let mut channels = self.last_pinged.write().unwrap();
            (*channels).insert(msg.channel_id, pl);
        };
    }

    // Set the Listening party as started
    pub fn start_lp(&self, channel: &ChannelId) {
        let now = chrono::offset::Utc::now();
        let mut channels = self.last_pinged.write().unwrap();
        channels
            .entry(*channel)
            .and_modify(|lp_info| lp_info.started = Some(now));
    }
}

#[async_trait]
impl Module for LP {
    async fn add_dependencies(
        builder: HandlerBuilder,
    ) -> anyhow::Result<HandlerBuilder> {
        builder.module::<Spotify>().await
    }

    fn register_event_handlers(&self, handlers: &mut events::EventHandlers) {
        let this = self.clone();
        handlers.add_handler(move |ReadyPollStarted { channel }| {
            this.start_lp(channel);
        });
    }

    fn register_commands(
        &self,
        store: &mut CommandStore,
        _completions: &mut CompletionStore,
    ) {
        store.register::<CurrentLP>();
        store.register::<JoinLP>();
    }

    async fn init(_m: &ModuleMap) -> anyhow::Result<Self> {
        Ok(Self::new())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn match_spotify_album_parses() {
        let aid = "4ddRx20FxcGU2ZJhateVym";

        let uris: &'static [&'static str] = &[
            "https://open.spotify.com/album/4ddRx20FxcGU2ZJhateVym", // regular
            "https://open.spotify.com/album/4ddRx20FxcGU2ZJhateVym\
                ?si=RQNX_vP_SN6Ct4haVZeHDA", // ?si=
            "https://open.spotify.com/intl-de/album\
             /4ddRx20FxcGU2ZJhateVym", // intl
            "https://open.spotify.com/intl-de/album\
             /4ddRx20FxcGU2ZJhateVym?si=RQNX_vP_SN6Ct4haVZeHDA", // intl + ?si=
        ];

        for uri in uris {
            assert_eq!(match_spotify_album(uri), Some(aid));
        }
    }

    #[test]
    fn match_spotify_playlist_parses() {
        let pid = "5Yy6oc82tIR8k25BdHcsdq";

        let uris: &'static [&'static str] = &[
            //regular
            "https://open.spotify.com/playlist/5Yy6oc82tIR8k25BdHcsdq",
            // ?si=
            "https://open.spotify.com/playlist/5Yy6oc82tIR8k25BdHcsdq\
                 ?si=574a09e801af4003",
            // intl
            "https://open.spotify.com/intl-de/playlist/5Yy6oc82tIR8k25BdHcsdq",
            // intl + ?si=
            "https://open.spotify.com/intl-de/playlist/5Yy6oc82tIR8k25BdHcsdq\
                 ?si=574a09e801af4003",
        ];

        for uri in uris {
            assert_eq!(match_spotify_playlist(uri), Some(pid));
        }
    }
}
