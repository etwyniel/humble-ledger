use anyhow::anyhow;
use reqwest::{Client, Url};
use scraper::{Html, Selector};
use serenity::async_trait;

use crate::album::{Album, AlbumProvider};

const SEARCH_URL: &str = "https://bandcamp.com/search";

fn contents(html: &Html, selector: &Selector) -> Option<String> {
    Some(
        html.select(selector)
            .next()?
            .text()
            .next()?
            .trim()
            .to_string(),
    )
}

pub struct Bandcamp {
    client: Client,
}

#[async_trait]
impl AlbumProvider for Bandcamp {
    fn id(&self) -> &'static str {
        "bandcamp"
    }

    async fn get_from_url(&self, url: &str) -> anyhow::Result<Album> {
        let mut url = Url::parse(url)?;
        url.query_pairs_mut().clear();
        let page = self.client.get(url.clone()).send().await?.text().await?;
        let html = Html::parse_document(&page);

        let title_selector = Selector::parse(".trackTitle").unwrap();
        let title = contents(&html, &title_selector).ok_or_else(|| anyhow!("Not an album page"))?;

        let artist_selector = Selector::parse("#name-section>h3>span>a").unwrap();
        let artist = contents(&html, &artist_selector)
            .ok_or_else(|| anyhow!("Could not find album artist"))?;

        Ok(Album {
            name: title,
            artist,
        })
    }

    async fn query_album(&self, q: &str) -> anyhow::Result<Album> {
        let mut query_url = Url::parse(SEARCH_URL).unwrap();
        query_url
            .query_pairs_mut()
            .append_pair("q", q)
            .append_pair("item_type", "a");
        let page = self.client.get(query_url).send().await?.text().await?;

        let url_selector = Selector::parse(".result-info>.heading>a").unwrap();
        let url = Html::parse_document(&page)
            .select(&url_selector)
            .next()
            .ok_or_else(|| anyhow!("Not found"))?
            .value()
            .attr("href")
            .ok_or_else(|| anyhow!("Not found"))?
            .to_string();
        self.get_from_url(&url).await
    }

    fn url_matches(&self, url: &str) -> bool {
        url.starts_with("https://") && url.contains(".bandcamp.com")
    }
}

impl Bandcamp {
    pub fn new() -> Self {
        Bandcamp {
            client: Client::new(),
        }
    }
}
