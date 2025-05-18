use anyhow::Context as _;
use futures_util::stream::{StreamExt, TryStreamExt};
use once_cell::sync::Lazy;
use regex::Regex;
use rspotify::clients::BaseClient;
use rspotify::model::{FullEpisode, FullTrack, PlayableItem, PlaylistItem};
use serenity::all::{GuildId, RoleId};
use serenity::builder::CreateEmbed;
use serenity::model::prelude::CommandInteraction;
use serenity::model::prelude::{ChannelId, Message};
use serenity::{async_trait, prelude::Context};
use serenity_command::{BotCommand, CommandResponse, ResponseType};
use serenity_command_derive::Command;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

use serenity_command_handler::{events, RegisterableModule}; // serenity-command-handler, for hooking

use serenity_command_handler::modules::polls::ReadyPollStarted;
use serenity_command_handler::modules::Spotify;

use serenity_command_handler::{
    CommandStore, CompletionStore, Handler, HandlerBuilder, Module, ModuleMap,
};

#[derive(Debug)]
pub struct TrackInfo {
    /// Position in album/playlist
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

/// Stored information about a listening party in a channel
#[derive(Debug)]
pub struct LPInfo {
    playlist: PlaylistInfo,
    tracks: Vec<TrackInfo>,
    /// If and when the listening party has started
    started: Option<chrono::DateTime<chrono::Utc>>,
}

impl LPInfo {
    /// Look up an album from a spotify ID
    async fn from_spotify_album_id<C: BaseClient>(
        client: &C,
        album_id_str: &str,
    ) -> anyhow::Result<Self> {
        let album_id =
            rspotify::model::AlbumId::from_id(album_id_str).context("trying to parse album ID")?;

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
    /// Look up a playlist from a spotify ID
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
                uri: playlist.external_urls.get("spotify").map(|s| s.to_owned()),
            },
            tracks,
            started: None,
        })
    }

    /// Find spotify album or playlist in chat line and fetch info
    async fn from_match_string<C: BaseClient>(
        client: &C,
        string: &str,
    ) -> anyhow::Result<Option<Self>> {
        if let Some(aid) = match_spotify_album(string) {
            return Ok(Some(Self::from_spotify_album_id(client, aid).await?));
        }
        if let Some(pid) = match_spotify_playlist(string) {
            return Ok(Some(Self::from_spotify_playlist_id(client, pid).await?));
        }
        return Ok(None);
    }
}

/// State of the listening party
enum PlayState<'a> {
    NotStarted,
    Finished(
        /// How long ago has it finished
        chrono::Duration,
    ),
    Playing {
        track: &'a TrackInfo,
        /// What is the current position in the track
        position: chrono::Duration,
    },
}

/// Turn a string into a markdown link if an URI is available.
/// e.g. [mysong](htps://open.spotify.com/track/abcxyz)
fn maybe_uri<S: AsRef<str>, T: AsRef<str>>(text: T, mb_uri: Option<S>) -> String {
    match mb_uri.as_ref() {
        None => text.as_ref().to_string(),
        Some(uri) => format!("[{}]({})", text.as_ref(), uri.as_ref()),
    }
}

impl LPInfo {
    /// Calculate which track is playing `offset` seconds from now
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
        // = how long ago the playlist finished
        PlayState::Finished(remain)
    }

    /// Build discord embed for lp_info
    fn build_info_embed(&self) -> CreateEmbed {
        let (lp_name, lp_id) = match &self.playlist {
            PlaylistInfo::AlbumInfo {
                id,
                artist,
                name,
                uri,
            } => {
                let album_name = maybe_uri(format!("{artist} - {name}"), uri.as_ref());
                (format!("**Album**: {album_name}"), id.clone())
            }
            PlaylistInfo::PlaylistInfo { id, name, uri } => {
                let playlist_name = maybe_uri(name, uri.as_ref());
                (format!("**Playlist**: {playlist_name}"), id.clone())
            }
        };
        let playlist_duration = self.tracks.iter().map(|t| t.duration).sum();
        let mut embed = CreateEmbed::new().description(format!(
            "{} - \\[{}\\]",
            lp_name,
            display_duration(playlist_duration),
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
                let playlist_end = (self.started.unwrap() + playlist_duration).timestamp();
                embed = embed
                    .title("Listening Party in full swing! Join in!")
                    .field(
                        "",
                        format!(
                            "**Started**: <t:{}:t> (<t:{}:R>)\n\
                                    **Ends:** <t:{}:t> ",
                            self.started.unwrap().timestamp(),
                            self.started.unwrap().timestamp(),
                            playlist_end
                        ),
                        true,
                    )
                    .field(
                        "Now playing",
                        format!(
                            "Track {} - {} - [{}]\nTrack started <t:{}:R>",
                            track.number,
                            maybe_uri(&track.name, track_uri_ctx.as_ref()),
                            display_duration(track.duration),
                            (now - position).timestamp(),
                        ),
                        false,
                    );
            }
        }
        embed
    }

    /// Build discord embed for lp_join
    fn build_join_embed(&self, offset: chrono::Duration) -> CreateEmbed {
        let lp_id = match &self.playlist {
            PlaylistInfo::AlbumInfo { id, .. } | PlaylistInfo::PlaylistInfo { id, .. } => {
                id.clone()
            }
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
                    "Select track",
                    format!(
                        "{} - {} - ({})\nGo to position **{}**\n Start playback:\
                         <t:{}:R>",
                        track.number,
                        maybe_uri(&track.name, track_uri_ctx.as_ref()),
                        display_duration(track.duration),
                        display_duration(position),
                        (now + offset).timestamp()
                    ),
                    true,
                );
            }
        }
        embed
    }
}

/// Format Duration as [hh:]mm:ss
fn display_duration(duration: chrono::Duration) -> String {
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

/// Regex to identity spotify album URIs and extract album id
static SPOTIFY_ALBUM_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(
        "\\bhttps://open.spotify.com(?:/intl-[a-z]+)?\
            /album/([a-zA-Z0-9]+)(?:\\?[a-zA-Z?=&]*)?\\b",
    )
    .unwrap()
});

/// Find spotify playlist URI and extract the album ID
fn match_spotify_album(string: &str) -> Option<&str> {
    SPOTIFY_ALBUM_RE
        .captures(string.as_ref())
        .map(|caps| caps.get(1).unwrap().as_str())
}

/// Regex to identity spotify playlist URIs and extract album id
const SPOTIFY_PLAYLIST_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(
        "\\bhttps://open.spotify.com(?:/intl-[a-z]+)?\
           /playlist/([a-zA-Z0-9]+)(?:\\?[a-zA-Z?=&]*)?\\b",
    )
    .unwrap()
});

/// Find spotify playlist URI and extract the album ID
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
        _ctx: &Context,
        interaction: &CommandInteraction,
    ) -> anyhow::Result<CommandResponse> {
        let msg: ResponseType = {
            // Find last LP
            let lps = data.module::<ModLPInfo>().unwrap().last_pinged.read().await;
            let lp = lps.get(&interaction.channel_id);
            match lp {
                None => "There is no listening party at the moment.".into(),

                Some(lpinfo) => lpinfo.build_info_embed().into(),
            }
        };

        if self.visible.unwrap_or(false) {
            CommandResponse::public(msg)
        } else {
            CommandResponse::private(msg)
        }
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
        _ctx: &Context,
        interaction: &CommandInteraction,
    ) -> anyhow::Result<CommandResponse> {
        let offset = chrono::Duration::seconds(self.offset.unwrap_or(15) as i64);
        // Find last LP
        let lps = data.module::<ModLPInfo>().unwrap().last_pinged.read().await;
        let lp = lps.get(&interaction.channel_id);
        match lp {
            None => CommandResponse::private("There is no listening party at the moment."),
            Some(lpinfo) => CommandResponse::private(lpinfo.build_join_embed(offset)),
        }
    }
}

pub struct ModLPInfo {
    last_pinged: Arc<RwLock<HashMap<ChannelId, LPInfo>>>,
    lp_roles: Arc<RwLock<HashMap<GuildId, Vec<RoleId>>>>,
}

impl Clone for ModLPInfo {
    fn clone(&self) -> Self {
        ModLPInfo {
            last_pinged: Arc::clone(&self.last_pinged),
            lp_roles: Arc::clone(&self.lp_roles),
        }
    }
}

// Roles used for pinging listening parties
const LP_ROLES: &'static [&'static str] = &[&"Listening Party", &"Impromptu Listening Party"];

impl ModLPInfo {
    pub fn new() -> Self {
        ModLPInfo {
            last_pinged: Default::default(),
            lp_roles: Default::default(),
        }
    }

    // Handle messages to remember the last pinged album
    //
    // We consider a message a LP ping if if mentions one of the LP roles
    // and it contains a spotify playlist or album link
    pub async fn handle_message<C: BaseClient>(&self, client: &C, ctx: &Context, msg: &Message) {
        let msg_txt: &str = &msg.content;

        // Check if the specified roles were mentioned
        let Some(guild_id) = msg.guild_id else { return };
        if self.lp_roles.read().await.contains_key(&guild_id) {
            let roles = ctx.http.get_guild_roles(guild_id).await.unwrap();
            let lp_roles = roles
                .iter()
                .filter(|role| LP_ROLES.contains(&role.name.as_ref()))
                .map(|role| role.id)
                .collect();
            self.lp_roles.write().await.insert(guild_id, lp_roles);
        }
        let lp_roles_map = self.lp_roles.read().await;
        let Some(roles) = lp_roles_map.get(&guild_id) else {
            return;
        };
        if msg
            .mention_roles
            .iter()
            // Resolve ID to role
            .any(|rid| roles.contains(rid))
        {
            let pl = match LPInfo::from_match_string(client, msg_txt).await {
                Err(e) => {
                    eprintln!("Error resolving spotify link: {}", e);
                    return;
                }
                Ok(Some(pl)) => {
                    // Collect info to log
                    let guild_name = match msg.guild_id {
                        Some(guild) => guild
                            .to_partial_guild(&ctx.http)
                            .await
                            .map(|guild| format!("[{}] ", &guild.name))
                            .unwrap_or_default(),
                        None => String::new(),
                    };
                    let username = &msg.author.name;
                    let pinged = match &pl.playlist {
                        PlaylistInfo::AlbumInfo {
                            id, artist, name, ..
                        } => format!("{id} ({artist} - {name})"),
                        PlaylistInfo::PlaylistInfo { id, name, .. } => {
                            format!("{id} ({name})")
                        }
                    };
                    eprintln!("{guild_name}{username}: Pinged Listening Party: {pinged}");
                    pl
                }
                Ok(None) => return,
            };
            // Store album/playlist in channel info
            let mut channels = self.last_pinged.write().await;
            (*channels).insert(msg.channel_id, pl);
        };
    }

    // Set the Listening party as started
    pub async fn start_lp(&self, channel: &ChannelId) {
        let now = chrono::offset::Utc::now();
        let mut channels = self.last_pinged.write().await;
        channels
            .entry(*channel)
            .and_modify(|lp_info| lp_info.started = Some(now));
    }
}

#[async_trait]
impl Module for ModLPInfo {
    fn register_event_handlers(&self, handlers: &mut events::EventHandlers) {
        let that = self.clone();
        handlers.add_handler(move |ReadyPollStarted { channel }| {
            let this = that.clone();
            let c = *channel;
            Box::pin(async move {
                this.start_lp(&c).await;
            })
        });
    }

    fn register_commands(&self, store: &mut CommandStore, _completions: &mut CompletionStore) {
        store.register::<CurrentLP>();
        store.register::<JoinLP>();
    }
}

impl RegisterableModule for ModLPInfo {
    async fn add_dependencies(builder: HandlerBuilder) -> anyhow::Result<HandlerBuilder> {
        builder.module::<Spotify>().await
    }

    async fn init(_m: &ModuleMap) -> anyhow::Result<Self> {
        Ok(Self::new())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Generate test functions for parsing uris
    macro_rules! test_parser {
        ($parser:ident to $id:literal {
            $($uri:literal as $name:ident),*}) => {$(
            #[test]
            fn $name() { assert_eq!($parser($uri) , Some($id)); }
        )*}
    }

    mod match_spotify_album {
        use super::*;
        test_parser! {
            match_spotify_album to "4ddRx20FxcGU2ZJhateVym" {
                "https://open.spotify.com/album/4ddRx20FxcGU2ZJhateVym"
                    as regular,
                "https://open.spotify.com/album/4ddRx20FxcGU2ZJhateVym\
                ?si=RQNX_vP_SN6Ct4haVZeHDA" as si,
                "https://open.spotify.com/intl-de/album\
                 /4ddRx20FxcGU2ZJhateVym" as intl,
                "https://open.spotify.com/intl-de/album\
                 /4ddRx20FxcGU2ZJhateVym?si=RQNX_vP_SN6Ct4haVZeHDA" as intl_si
            }
        }
    }

    mod match_spotify_playlist {
        use super::*;
        test_parser! {
            match_spotify_playlist to "5Yy6oc82tIR8k25BdHcsdq" {
            "https://open.spotify.com/playlist/5Yy6oc82tIR8k25BdHcsdq"
                    as regular,
            "https://open.spotify.com/playlist/5Yy6oc82tIR8k25BdHcsdq\
                 ?si=574a09e801af4003" as si,
            "https://open.spotify.com/intl-de/playlist/5Yy6oc82tIR8k25BdHcsdq"
                    as intl,
            "https://open.spotify.com/intl-de/playlist/5Yy6oc82tIR8k25BdHcsdq\
             ?si=574a09e801af4003"
                    as intl_si
            }
        }
    }
}
