use anyhow::{anyhow, bail};
use rspotify::{
    clients::BaseClient,
    model::{AlbumId, FullTrack, Id, SearchType, SimplifiedArtist, TrackId},
    ClientCredsSpotify, Config, Credentials,
};
use serenity::async_trait;

use crate::album::{Album, AlbumProvider};

const ALBUM_URL_START: &str = "https://open.spotify.com/album/";
const TRACK_URL_START: &str = "https://open.spotify.com/track/";

pub struct Spotify {
    client: ClientCredsSpotify,
}

pub fn artists_to_string(artists: &[SimplifiedArtist]) -> String {
    artists
        .iter()
        .map(|a| a.name.as_ref())
        .collect::<Vec<_>>()
        .join(", ")
}

impl Spotify {
    pub async fn new() -> anyhow::Result<Self> {
        let creds = Credentials::from_env().ok_or_else(|| anyhow!("No spotify credentials"))?;
        let config = Config {
            token_refreshing: true,
            ..Default::default()
        };
        let mut spotify = ClientCredsSpotify::with_config(creds, config);

        // Obtaining the access token
        spotify.request_token().await?;
        Ok(Spotify { client: spotify })
    }

    pub async fn get_album_from_id(&self, id: &str) -> anyhow::Result<Album> {
        let album = self.client.album(&AlbumId::from_id(id)?).await?;
        let name = album.name.clone();
        let artist = artists_to_string(&album.artists);
        let url = album.id.url();
        Ok(Album { name, artist, url })
    }

    pub async fn get_song_from_id(&self, id: &str) -> anyhow::Result<FullTrack> {
        Ok(self.client.track(&TrackId::from_id(id)?).await?)
    }

    pub async fn get_song_from_url(&self, url: &str) -> anyhow::Result<FullTrack> {
        if let Some(id) = url.strip_prefix(TRACK_URL_START) {
            self.get_song_from_id(id.split('?').next().unwrap()).await
        } else {
            bail!("Invalid spotify url")
        }
    }

    pub async fn query_albums(&self, query: &str) -> anyhow::Result<Vec<(String, String)>> {
        let res = self
            .client
            .search(query, &SearchType::Album, None, None, Some(10), None)
            .await?;
        if let rspotify::model::SearchResult::Albums(albums) = res {
            Ok(albums
                .items
                .into_iter()
                .map(|a| {
                    (
                        format!(
                            "{} - {}",
                            a.artists
                                .into_iter()
                                .next()
                                .map(|ar| ar.name)
                                .unwrap_or_default(),
                            a.name,
                        ),
                        a.id.map(|id| id.url()).unwrap_or_default(),
                    )
                })
                .collect())
        } else {
            Err(anyhow!("Not an album"))
        }
    }

    pub async fn query_songs(&self, query: &str) -> anyhow::Result<Vec<(String, String)>> {
        let res = self
            .client
            .search(query, &SearchType::Track, None, None, Some(10), None)
            .await?;
        if let rspotify::model::SearchResult::Tracks(songs) = res {
            Ok(songs
                .items
                .into_iter()
                .map(|a| {
                    (
                        format!(
                            "{} - {}",
                            a.artists
                                .into_iter()
                                .next()
                                .map(|ar| ar.name)
                                .unwrap_or_default(),
                            a.name,
                        ),
                        a.id.map(|id| id.url()).unwrap_or_default(),
                    )
                })
                .collect())
        } else {
            Err(anyhow!("Not an album"))
        }
    }
}

#[async_trait]
impl AlbumProvider for Spotify {
    fn id(&self) -> &'static str {
        "spotify"
    }

    async fn get_from_url(&self, url: &str) -> anyhow::Result<Album> {
        if let Some(id) = url.strip_prefix(ALBUM_URL_START) {
            self.get_album_from_id(id.split('?').next().unwrap()).await
        } else {
            bail!("Invalid spotify url")
        }
    }

    fn url_matches(&self, url: &str) -> bool {
        url.starts_with(ALBUM_URL_START)
    }

    async fn query_album(&self, query: &str) -> anyhow::Result<Album> {
        let res = self
            .client
            .search(query, &SearchType::Album, None, None, Some(1), None)
            .await?;
        if let rspotify::model::SearchResult::Albums(albums) = res {
            Ok(albums
                .items
                .first()
                .map(|a| Album {
                    name: a.name.clone(),
                    artist: a.artists.first().unwrap().name.clone(),
                    url: a.href.clone().unwrap(),
                })
                .ok_or_else(|| anyhow!("Not found"))?)
        } else {
            Err(anyhow!("Not an album"))
        }
    }
}
