use std::str::FromStr;

use anyhow::bail;
use chrono::Local;
use google_sheets4::api::ValueRange;
use serenity::{
    async_trait, builder::CreateApplicationCommandOption, client::Context,
    model::prelude::interaction::application_command::ApplicationCommandInteraction,
};

use crate::Handler;
use serenity_command::{BotCommand, CommandResponse};
use serenity_command_derive::Command;

const FORM_SPREADSHEET: &str = "10lpL3w0Fm2TFcMdVhNNxfTvOONWI0BL4aP6-tW83RQQ";
const SUBMISSIONS_RANGE: &str = "A:Z";

#[derive(Debug, PartialEq, Eq, PartialOrd, Ord, Clone, Copy)]
#[repr(u8)]
enum AlbumCategory {
    Rock = 1,
    Metal = 2,
    Other = 3,
}

use AlbumCategory::*;

impl FromStr for AlbumCategory {
    type Err = anyhow::Error;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(match s {
            "Rock" => Rock,
            "Metal" => Metal,
            "Other" => Other,
            _ => bail!("Invalid category: {}", s),
        })
    }
}

const CATEGORIES: [AlbumCategory; 3] = [Rock, Metal, Other];

#[derive(Command)]
#[cmd(
    name = "submit_album_club",
    desc = "Submit an album to the weekly Album Club"
)]
pub struct SubmitAlbum {
    #[cmd(desc = "Category to submit to")]
    category: String,
    #[cmd(
        desc = "Link to the album (spotify/bandcamp/youtube preferred)",
        autocomplete
    )]
    link: String,
}

#[async_trait]
impl BotCommand for SubmitAlbum {
    type Data = Handler;

    async fn run(
        self,
        handler: &Handler,
        _ctx: &Context,
        interaction: &ApplicationCommandInteraction,
    ) -> anyhow::Result<serenity_command::CommandResponse> {
        let category: AlbumCategory = self.category.parse()?;
        let user = &interaction.user;
        let user_handle = format!("{}#{:04}", &user.name, user.discriminator);
        let mut values = vec!["".to_string(); 8];
        let now = Local::now();
        let timestamp = format!("{}", now.format("%m/%d/%Y %H:%M:%S"));
        let album_info =
            if let Some(p) = handler.providers.iter().find(|p| p.url_matches(&self.link)) {
                Some(p.get_from_url(&self.link).await?).map(|info| info.format_name())
            } else {
                None
            };
        values[0] = timestamp;
        values[1] = user_handle;
        let offset = category as usize * 2;
        if let Some(info) = album_info.as_deref() {
            values[offset].push_str(info)
        }
        values[offset + 1].push_str(&self.link);

        let request = ValueRange {
            major_dimension: None,
            range: Some(SUBMISSIONS_RANGE.to_string()),
            values: Some(vec![values]),
        };
        handler
            .sheets_client
            .spreadsheets()
            .values_append(request, FORM_SPREADSHEET, SUBMISSIONS_RANGE)
            .value_input_option("USER_ENTERED")
            .doit()
            .await?;
        let resp = format!(
            "Submitted {} to the {:?} category",
            album_info.as_deref().unwrap_or(&self.link),
            category,
        );
        Ok(CommandResponse::Private(resp))
    }

    fn setup_options(opt_name: &'static str, opt: &mut CreateApplicationCommandOption) {
        if opt_name == "category" {
            for cat in CATEGORIES.as_ref() {
                let cat_str = format!("{:?}", cat);
                opt.add_string_choice(&cat_str, &cat_str);
            }
        }
    }
}
