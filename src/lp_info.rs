use crate::serenity;
use anyhow::Context as _;
use chrono::{DateTime, Utc};
use futures_util::stream::TryStreamExt;
use once_cell::sync::Lazy;
use regex::Regex;
use rspotify::clients::BaseClient;
use rspotify::model::{FullEpisode, FullTrack, PlayableItem, PlaylistItem};
use serenity::all::{GuildId, RoleId};
use serenity::builder::CreateEmbed;
use serenity::model::prelude::CommandInteraction;
use serenity::model::prelude::Message;
use serenity::{async_trait, prelude::Context};
use serenity_command::{CommandResponse, ResponseType, args, command};
use serenity_command_handler::modules::Spotify;
use serenity_command_handler::serenity::all::{GenericChannelId, Http, MessageId};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{OnceCell, RwLock};

use serenity_command_handler::{CommandConst, RegisterableModule, Storer, events}; // serenity-command-handler, for hooking

use serenity_command_handler::modules::polls::ReadyPollStarted;

use serenity_command_handler::{Handler, HandlerBuilder, Module, ModuleMap};

#[derive(Debug)]
pub struct TrackInfo {
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
    /// when the listening party has started
    started: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug)]
pub enum PingedLp {
    Pinged(MessageId),
    Started(LPInfo),
}

impl LPInfo {
    /// Look up an album from a spotify ID
    async fn from_spotify_album_id<C: BaseClient>(
        client: &C,
        album_id_str: &str,
        started: DateTime<Utc>,
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
            .map_ok(|track| TrackInfo {
                name: track.name.to_string(),
                duration: track.duration,
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
            started,
        })
    }
    /// Look up a playlist from a spotify ID
    async fn from_spotify_playlist_id<C: BaseClient>(
        client: &C,
        album_id_str: &str,
        started: DateTime<Utc>,
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
            .filter_map(|item| {
                item.track.as_ref().and_then(|track| match track {
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
                    }) => Some(TrackInfo {
                        name: name.to_string(),
                        duration: *duration,
                        uri: external_urls.get("spotify").map(|s| s.to_owned()),
                    }),
                    _ => None,
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
            started,
        })
    }

    /// Find spotify album or playlist in chat line and fetch info
    async fn from_match_string<C: BaseClient>(
        client: &C,
        string: &str,
        started: DateTime<Utc>,
    ) -> anyhow::Result<Option<Self>> {
        if let Some(aid) = match_spotify_album(string) {
            return Ok(Some(
                Self::from_spotify_album_id(client, aid, started).await?,
            ));
        }
        if let Some(pid) = match_spotify_playlist(string) {
            return Ok(Some(
                Self::from_spotify_playlist_id(client, pid, started).await?,
            ));
        }
        Ok(None)
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
        number: usize,
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
    fn now_playing(&self, offset: chrono::Duration) -> PlayState<'_> {
        let started = self.started;
        let now = chrono::offset::Utc::now();
        if started > now {
            eprintln!(
                "LPInfo: Started timestamp in the future! started={} > now={}",
                started, now
            );
            return PlayState::NotStarted;
        }
        let mut remain = now - started + offset;
        for (n, track) in self.tracks.iter().enumerate() {
            if track.duration > remain {
                return PlayState::Playing {
                    number: n + 1,
                    track,
                    position: remain,
                };
            } else {
                remain -= track.duration;
            }
        }
        // We passed all the tracks
        // remain = now - started - sum(track duration)
        // = how long ago the playlist finished
        PlayState::Finished(remain)
    }

    /// Build discord embed for lp_info
    fn build_info_embed(&self) -> CreateEmbed<'static> {
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
                track,
                position,
                number,
                ..
            } => {
                let now = chrono::offset::Utc::now();
                let track_uri_ctx = track
                    .uri
                    .as_ref()
                    .map(|uri| format!("{}?context={}", uri, &lp_id));
                let playlist_end = (self.started + playlist_duration).timestamp();
                embed = embed
                    .title("Listening Party in full swing! Join in!")
                    .field(
                        "",
                        format!(
                            "**Started**: <t:{}:t> (<t:{}:R>)\n\
                                    **Ends:** <t:{}:t> ",
                            self.started.timestamp(),
                            self.started.timestamp(),
                            playlist_end
                        ),
                        true,
                    )
                    .field(
                        "Now playing",
                        format!(
                            "Track {} - {} - [{}]\nTrack started <t:{}:R>",
                            number,
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
    fn build_join_embed(&self, offset: chrono::Duration) -> CreateEmbed<'static> {
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
            PlayState::Playing {
                track,
                position,
                number,
            } => {
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
                        number,
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
static SPOTIFY_PLAYLIST_RE: Lazy<Regex> = Lazy::new(|| {
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

args!(CURRENT_LP_ARGS =
     "Should the answer be visible to everyone?"
    visible: Option<bool>,
);

const CURRENT_LP: CommandConst = CommandConst {
    description: "Should the answer be visible to everyone?",
    ..command!(/lp_info CURRENT_LP_ARGS: current_lp)
};

async fn current_lp(
    (visible,): CURRENT_LP_ARGS,
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
            Some(PingedLp::Pinged(_)) => "Listening Party has not started yet.".into(),
            Some(PingedLp::Started(lpinfo)) => lpinfo.build_info_embed().into(),
        }
    };

    if visible.unwrap_or(false) {
        CommandResponse::public(msg)
    } else {
        CommandResponse::private(msg)
    }
}

args!(LP_JOIN_ARGS =
     "Seconds to start playing"
    offset: Option<i64>,
);

const LP_JOIN: CommandConst = CommandConst {
    description: "Join a listening party (privately)",
    ..command!(/lp_join LP_JOIN_ARGS: lp_join)
};

async fn lp_join(
    (offset,): LP_JOIN_ARGS,
    data: &Handler,
    _ctx: &Context,
    interaction: &CommandInteraction,
) -> anyhow::Result<CommandResponse> {
    let offset = chrono::Duration::seconds(offset.unwrap_or(15));
    // Find last LP
    let lps = data.module::<ModLPInfo>().unwrap().last_pinged.read().await;
    let lp = lps.get(&interaction.channel_id);
    match lp {
        None => CommandResponse::private("There is no listening party at the moment."),
        Some(PingedLp::Pinged(_)) => {
            CommandResponse::private("Listening Party has not started yet.")
        }
        Some(PingedLp::Started(lpinfo)) => {
            CommandResponse::private(lpinfo.build_join_embed(offset))
        }
    }
}

pub struct ModLPInfo {
    last_pinged: Arc<RwLock<HashMap<GenericChannelId, PingedLp>>>,
    lp_roles: Arc<RwLock<HashMap<GuildId, Vec<RoleId>>>>,
    spotify: Arc<Spotify>,
    http: OnceCell<Arc<Http>>,
}

impl Clone for ModLPInfo {
    fn clone(&self) -> Self {
        ModLPInfo {
            last_pinged: Arc::clone(&self.last_pinged),
            lp_roles: Arc::clone(&self.lp_roles),
            spotify: Arc::clone(&self.spotify),
            http: self.http.clone(),
        }
    }
}

// Roles used for pinging listening parties
const LP_ROLES: &[&str] = &["Listening Party", "Impromptu Listening Party"];

impl ModLPInfo {
    pub fn new(spotify: Arc<Spotify>) -> Self {
        ModLPInfo {
            last_pinged: Default::default(),
            lp_roles: Default::default(),
            spotify,
            http: Default::default(),
        }
    }

    // Handle messages to remember the last pinged album
    //
    // We consider a message a LP ping if if mentions one of the LP roles
    // and it contains a spotify playlist or album link
    pub async fn handle_message(&self, ctx: &Context, msg: &Message) {
        if msg.mention_roles.is_empty() {
            return;
        }

        // Check if the specified roles were mentioned
        let Some(guild_id) = msg.guild_id else { return };
        if !self.lp_roles.read().await.contains_key(&guild_id) {
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
        if !msg
            .mention_roles
            .iter()
            // Resolve ID to role
            .any(|rid| roles.contains(rid))
        {
            return;
        }
        // Store LP message ID in channel info
        let mut channels = self.last_pinged.write().await;
        let pl = PingedLp::Pinged(msg.id);
        (*channels).insert(msg.channel_id, pl);
    }

    // Set the Listening party as started
    pub async fn start_lp(&self, channel: GenericChannelId) {
        let last_pinged = Arc::clone(&self.last_pinged);
        let spotify = Arc::clone(&self.spotify);
        let http = Arc::clone(self.http.get().unwrap());
        tokio::spawn(async move {
            let now = chrono::offset::Utc::now();
            let mut channels = last_pinged.write().await;
            let Some(entry) = channels.get_mut(&channel) else {
                return;
            };
            let PingedLp::Pinged(msg_id) = &*entry else {
                return;
            };
            let message = http.get_message(channel, *msg_id).await.unwrap();
            let pl = match LPInfo::from_match_string(&spotify.client, &message.content, now).await {
                Err(e) => {
                    eprintln!("Error resolving spotify link: {}", e);
                    return;
                }
                Ok(Some(pl)) => pl,
                Ok(None) => return,
            };
            // Collect info to log
            let guild_name = match message.guild_id {
                Some(guild) => guild
                    .to_partial_guild(http)
                    .await
                    .map(|guild| format!("[{}] ", &guild.name))
                    .unwrap_or_default(),
                None => String::new(),
            };
            let username = &message.author.name;
            let pinged = match &pl.playlist {
                PlaylistInfo::AlbumInfo {
                    id, artist, name, ..
                } => format!("{id} ({artist} - {name})"),
                PlaylistInfo::PlaylistInfo { id, name, .. } => {
                    format!("{id} ({name})")
                }
            };
            eprintln!("{guild_name}{username}: Pinged Listening Party: {pinged}");
            // Store album/playlist in channel info
            *entry = PingedLp::Started(pl);
        });
    }
}

#[async_trait]
impl Module for ModLPInfo {
    fn register_event_handlers(self: Arc<Self>, handlers: &mut events::EventHandlers) {
        let that = self.clone();
        handlers.add_handler(move |ReadyPollStarted { channel }| {
            let this = that.clone();
            let c = *channel;
            Box::pin(async move {
                this.start_lp(c).await;
            })
        });
    }

    fn register_commands(&self, store: &mut dyn Storer) {
        store.register(CURRENT_LP);
        store.register(LP_JOIN);
    }

    fn start(&self, ctx: &Context, _: &serenity::model::gateway::Ready) {
        let http = Arc::clone(&ctx.http);
        self.http.set(http).unwrap();
    }
}

impl RegisterableModule for ModLPInfo {
    async fn add_dependencies(builder: HandlerBuilder) -> anyhow::Result<HandlerBuilder> {
        builder.module::<Spotify>().await
    }

    async fn init(m: &ModuleMap) -> anyhow::Result<Self> {
        let spotify = m.module_arc().unwrap();
        Ok(Self::new(spotify))
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
