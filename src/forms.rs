use std::{cmp::Ordering, time::Duration};

use anyhow::{anyhow, bail, Context as _};
use chrono::Local;
use fallible_iterator::FallibleIterator;
use hyper::{client::HttpConnector, Body, Method, Request, StatusCode};
use hyper_tls::HttpsConnector;
use itertools::Itertools;
use regex::Regex;
use rspotify::prelude::Id;
use rusqlite::{params, Connection};
use serde_derive::{Deserialize, Serialize};
use serenity::{
    async_trait,
    builder::{CreateApplicationCommand, CreateEmbed},
    model::{
        prelude::{
            command::CommandOptionType,
            interaction::application_command::{
                ApplicationCommandInteraction, CommandDataOptionValue,
            },
            GuildId,
        },
        Permissions,
    },
    prelude::Context,
};
use serenity_command::{BotCommand, CommandResponse};
use serenity_command_derive::Command;
use yup_oauth2::authenticator::Authenticator;

use crate::{spotify, Handler};

#[derive(Deserialize, Debug)]
pub struct Form {
    #[serde(rename = "formId")]
    pub id: String,
    pub info: Info,
    pub items: Vec<Item>,
    #[serde(rename = "responderUri")]
    pub uri: String,
    #[serde(rename = "linkedSheetId")]
    pub linked_sheet_id: Option<String>,
}

#[derive(Deserialize, Debug)]
pub struct Info {
    pub title: Option<String>,
    pub description: Option<String>,
}

#[derive(Deserialize, Debug)]
pub struct Item {
    #[serde(rename = "itemId")]
    pub id: String,
    pub title: Option<String>,
    pub description: Option<String>,

    #[serde(rename = "questionItem")]
    pub question: Option<QuestionItem>,
    #[serde(rename = "questionGroupItem")]
    pub question_group: Option<QuestionGroupItem>,
    #[serde(rename = "pageBreakItem")]
    pub page_break: Option<PageBreakItem>,
    #[serde(rename = "textItem")]
    pub text: Option<TextItem>,
    #[serde(rename = "imageItem")]
    pub image: Option<ImageItem>,
    #[serde(rename = "videoItem")]
    pub video: Option<VideoItem>,
}

#[derive(Deserialize, Debug)]
pub struct QuestionItem {
    pub question: Question,
}

#[derive(Deserialize, Debug)]
pub struct QuestionGroupItem {
    pub questions: Vec<Question>,
}

#[derive(Deserialize, Debug)]
pub struct PageBreakItem {}

#[derive(Deserialize, Debug)]
pub struct TextItem {}

#[derive(Deserialize, Debug)]
pub struct ImageItem {}

#[derive(Deserialize, Debug)]
pub struct VideoItem {}

#[derive(Deserialize, Debug)]
pub struct Question {
    #[serde(rename = "questionId")]
    pub id: String,
    #[serde(default)]
    pub required: bool,

    #[serde(rename = "choiceQuestion")]
    pub choice: Option<ChoiceQuestion>,
    #[serde(rename = "textQuestion")]
    pub text: Option<TextQuestion>,
    #[serde(rename = "scaleQuestion")]
    pub scale: Option<ScaleQuestion>,
    #[serde(rename = "dateQuestion")]
    pub date: Option<DateQuestion>,
    #[serde(rename = "timeQuestion")]
    pub time: Option<TimeQuestion>,
    #[serde(rename = "fileUploadQuestion")]
    pub file_upload: Option<FileUploadQuestion>,
    #[serde(rename = "rowQuestion")]
    pub row: Option<RowQuestion>,
}

#[derive(Deserialize, Debug, PartialEq, Eq)]
pub enum ChoiceType {
    #[serde(rename = "RADIO")]
    Radio,
    #[serde(rename = "CHECKBOX")]
    Checkbox,
    #[serde(rename = "DROP_DOWN")]
    DropDown,
}

#[derive(Deserialize, Debug)]
pub struct ChoiceQuestion {
    #[serde(rename = "type")]
    pub ty: ChoiceType,
    pub options: Vec<ChoiceOption>,
}

#[derive(Deserialize, Debug)]
pub struct ChoiceOption {
    #[serde(default)]
    pub value: String,
    #[serde(rename = "isOther", default)]
    pub is_other: bool,
}

#[derive(Deserialize, Debug)]
pub struct TextQuestion {}

#[derive(Deserialize, Debug)]
pub struct ScaleQuestion {
    pub low: i64,
    pub high: i64,
}

#[derive(Deserialize, Debug)]
pub struct DateQuestion {}

#[derive(Deserialize, Debug)]
pub struct TimeQuestion {}

#[derive(Deserialize, Debug)]
pub struct FileUploadQuestion {}

#[derive(Deserialize, Debug)]
pub struct RowQuestion {}

#[derive(Deserialize, Serialize, Debug)]
pub struct SimpleForm {
    pub id: String,
    pub title: String,
    pub questions: Vec<SimpleQuestion>,
    pub responder_uri: String,
    pub sheet_id: String,
}

#[derive(Deserialize, Serialize, Debug)]
pub struct SimpleQuestion {
    #[serde(default)]
    pub id: String,
    pub required: bool,
    pub title: String,
    pub ty: QuestionType,
}

#[derive(Deserialize, Serialize, Debug)]
pub enum QuestionType {
    Text,
    Choice(Vec<String>),
}

impl Item {
    pub fn to_simple(&self) -> Option<anyhow::Result<SimpleQuestion>> {
        let question = match &self.question {
            Some(q) => &q.question,
            _ => return None,
        };
        let title = match self.title.as_deref() {
            Some(title) => title.to_string(),
            None => return Some(Err(anyhow!("Question is missing a title"))),
        };
        let required = question.required;
        let ty = if question.text.is_some() {
            QuestionType::Text
        } else if let Some(choice) = question.choice.as_ref() {
            if choice.ty == ChoiceType::Checkbox {
                return Some(Err(anyhow!("Checkboxes are not supported")));
            }
            if choice.options.iter().any(|opt| opt.is_other) {
                return Some(Err(anyhow!("'Other' field is not supported")));
            }
            let values = choice.options.iter().map(|opt| opt.value.clone()).collect();
            QuestionType::Choice(values)
        } else {
            return Some(Err(anyhow!("Can only handle text or choice questions")));
        };
        Some(Ok(SimpleQuestion {
            id: question.id.clone(),
            required,
            title,
            ty,
        }))
    }
}

impl Form {
    pub fn to_simple(&self) -> anyhow::Result<SimpleForm> {
        let id = self.id.clone();
        let title = self
            .info
            .title
            .as_ref()
            .ok_or_else(|| anyhow!("Form is missing a title"))?
            .clone();
        let questions = self
            .items
            .iter()
            .filter_map(Item::to_simple)
            .collect::<anyhow::Result<Vec<_>>>()?;
        let responder_uri = self.uri.clone();
        let sheet_id = self
            .linked_sheet_id
            .as_ref()
            .ok_or_else(|| anyhow!("No linked spreadsheet"))?
            .clone();
        Ok(SimpleForm {
            id,
            title,
            questions,
            responder_uri,
            sheet_id,
        })
    }
}
pub fn sanitize_name(s: &str) -> String {
    let temp = s.chars().filter(|c| c.is_ascii()).collect::<String>();
    let it = temp
        .trim()
        .chars()
        .map(|c| {
            if c.is_whitespace() || "-+&./".contains(c) {
                '_'
            } else {
                c.to_ascii_lowercase()
            }
        })
        .filter(|&c| c.is_alphanumeric() || c == '_');
    let mut out = String::with_capacity(s.len());
    let mut prev_was_underscore = false;
    for c in it {
        if out.len() >= 32 {
            break;
        }
        if c == '_' {
            if !prev_was_underscore {
                prev_was_underscore = true;
                out.push(c)
            }
            continue;
        }
        prev_was_underscore = false;
        out.push(c);
    }
    out
}

impl SimpleForm {
    pub fn to_command(&self, command_name: &str) -> CreateApplicationCommand {
        let mut cmd = CreateApplicationCommand::default();
        cmd.name(sanitize_name(command_name))
            .description(&self.title);
        let mut questions = self.questions.iter().skip(1).collect::<Vec<_>>();
        questions.sort_by(|l, r| match (l.required, r.required) {
            (true, true) | (false, false) => Ordering::Equal,
            (false, true) => Ordering::Greater,
            (true, false) => Ordering::Less,
        });
        let mut autocomplete = false;
        for (i, q) in questions.iter().enumerate() {
            let sanitized = sanitize_name(&q.title);
            if let Some(next) = questions.get(i + 1) {
                let next_lower = next.title.to_lowercase();
                if matches!(q.ty, QuestionType::Text)
                    && (next_lower.contains("spotify") || next_lower.contains("link"))
                {
                    // q is most likely asking for the song artist and name, which we will retrieve
                    // using the song url
                    autocomplete = true;
                    continue;
                }
            }
            cmd.create_option(|opt| {
                opt.kind(CommandOptionType::String)
                    .name(&sanitized)
                    .description(&q.title)
                    .required(q.required)
                    .set_autocomplete(autocomplete);
                if let QuestionType::Choice(values) = &q.ty {
                    for v in values {
                        opt.add_string_choice(v, v);
                    }
                }
                opt
            });
            autocomplete = false;
        }
        cmd
    }
}

pub struct FormsClient {
    pub authenticator: Authenticator<HttpsConnector<HttpConnector>>,
    pub client: hyper::Client<HttpsConnector<HttpConnector>>,
}

impl FormsClient {
    pub async fn get_form(&self, form_id: &str) -> anyhow::Result<SimpleForm> {
        let token = self
            .authenticator
            .token(&["https://www.googleapis.com/auth/forms.body.readonly"])
            .await?;
        let req = Request::builder()
            .uri(format!("https://forms.googleapis.com/v1/forms/{}", form_id,))
            .header("Authorization", format!("Bearer {}", token.as_str()))
            .body(Body::empty())?;
        let resp = self.client.request(req).await?;
        if resp.status() != StatusCode::OK {
            bail!("Could not get form: status {}", resp.status());
        }
        let bytes = hyper::body::to_bytes(resp.into_body()).await?;
        let form: Form = serde_json::from_slice(&bytes)?;
        form.to_simple()
    }
}

pub struct FormCommand {
    pub guild_id: u64,
    pub command_name: String,
    pub command_id: u64,
    pub form: SimpleForm,
    pub submission_type: String,
}

#[derive(Command, Debug)]
#[cmd(
    name = "command_from_form",
    desc = "Create a submission command from a Google Form"
)]
pub struct CommandFromForm {
    #[cmd(desc = "The name of the command")]
    pub command_name: String,
    #[cmd(desc = "The edit id of the form to use (found in the url when editing it)")]
    pub form_id: String,
    #[cmd(desc = "Whether users will be submitting songs or albums")]
    pub submission_type: Option<String>,
}

#[async_trait]
impl BotCommand for CommandFromForm {
    type Data = Handler;
    const PERMISSIONS: Permissions = Permissions::MANAGE_EVENTS;

    async fn run(
        self,
        handler: &Handler,
        ctx: &Context,
        interaction: &ApplicationCommandInteraction,
    ) -> anyhow::Result<CommandResponse> {
        let guild_id = interaction
            .guild_id
            .ok_or_else(|| anyhow!("Must be run in a guild"))?;
        self.add_form(handler, ctx, guild_id).await
    }

    fn setup_options(
        opt_name: &'static str,
        opt: &mut serenity::builder::CreateApplicationCommandOption,
    ) {
        if opt_name == "submission_type" {
            opt.add_string_choice("song", "song");
            opt.add_string_choice("album", "album");
        }
    }
}

impl CommandFromForm {
    async fn add_form(
        mut self,
        handler: &Handler,
        ctx: &Context,
        guild_id: GuildId,
    ) -> anyhow::Result<CommandResponse> {
        let spreadsheet_url_re = Regex::new(r#"https://docs.google.com/forms/d/([^/]+)"#).unwrap();
        if let Some(cap) = spreadsheet_url_re.captures(&self.form_id) {
            self.form_id = cap.get(1).unwrap().as_str().to_string();
        }
        let form = handler.forms_client.get_form(&self.form_id).await?;
        let cmd = form.to_command(&self.command_name);
        let cmd = guild_id
            .create_application_command(&ctx.http, |c| {
                *c = cmd;
                c
            })
            .await?;
        let resp = format!("Created command </{}:{}>", &cmd.name, cmd.id.0);
        let form_json = serde_json::to_string(&form)?;
        let submission_type = self
            .submission_type
            .as_deref()
            .unwrap_or("song")
            .to_string();

        let db = handler.db.lock().await;
        db.execute(
            "INSERT INTO forms (guild_id, command_name, command_id, form, submission_type)
                 VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT (guild_id, command_name) DO UPDATE
                 SET command_id = ?3, form = ?4, submission_type = ?5
                 WHERE guild_id = ?1 AND command_name = ?2",
            params![guild_id.0, &cmd.name, cmd.id.0, form_json, &submission_type],
        )?;

        let command = FormCommand {
            guild_id: guild_id.0,
            command_name: cmd.name.clone(),
            command_id: cmd.id.0,
            form,
            submission_type,
        };
        let mut forms = handler.forms.write().await;
        if let Some(form) = forms
            .iter_mut()
            .find(|form| form.command_name == self.command_name)
        {
            *form = command;
        } else {
            forms.push(command);
        }
        Ok(CommandResponse::Public(resp))
    }
}

pub async fn check_forms(handler: &Handler, ctx: &Context) -> anyhow::Result<()> {
    let mut to_re_add = Vec::new();
    {
        for form in handler.forms.read().await.iter() {
            if form.form.questions[0].id.is_empty() {
                to_re_add.push((
                    form.guild_id,
                    form.command_name.clone(),
                    form.form.id.clone(),
                    form.submission_type.clone(),
                ));
            }
        }
    }
    for (guild_id, command_name, form_id, submission_type) in to_re_add {
        CommandFromForm {
            form_id,
            command_name,
            submission_type: Some(submission_type),
        }
        .add_form(handler, ctx, GuildId(guild_id))
        .await?;
    }
    Ok(())
}

#[derive(Command, Debug)]
#[cmd(name = "refresh_form_command", desc = "Refreshes a form command")]
pub struct RefreshFormCommand {
    #[cmd(desc = "The name of the command to refresh", autocomplete)]
    pub command_name: String,
}

#[async_trait]
impl BotCommand for RefreshFormCommand {
    type Data = Handler;
    const PERMISSIONS: Permissions = Permissions::MANAGE_EVENTS;

    async fn run(
        self,
        handler: &Handler,
        ctx: &Context,
        interaction: &ApplicationCommandInteraction,
    ) -> anyhow::Result<CommandResponse> {
        let guild_id = interaction
            .guild_id
            .ok_or_else(|| anyhow!("Must be run in a guild"))?
            .0;

        let (form, submission_type): (String, Option<String>) = {
            let db = handler.db.lock().await;
            db.query_row(
                "SELECT form, submission_type FROM forms WHERE guild_id = ?1 AND command_name = ?2",
                params![guild_id, &self.command_name],
                |row| Ok((row.get(0)?, row.get(1)?)),
            ).context(format!("Command /{} not found", &self.command_name))?
        };
        let form: SimpleForm = serde_json::from_slice(form.as_bytes())?;
        CommandFromForm {
            command_name: self.command_name,
            form_id: form.id,
            submission_type,
        }
        .run(handler, ctx, interaction)
        .await
    }
}

#[derive(Command, Debug)]
#[cmd(
    name = "delete_form_command",
    desc = "Delete a form submission command"
)]
pub struct DeleteFormCommand {
    #[cmd(desc = "The name of the command to delete", autocomplete)]
    pub command_name: String,
}

#[async_trait]
impl BotCommand for DeleteFormCommand {
    type Data = Handler;
    const PERMISSIONS: Permissions = Permissions::MANAGE_EVENTS;

    async fn run(
        self,
        handler: &Handler,
        ctx: &Context,
        interaction: &ApplicationCommandInteraction,
    ) -> anyhow::Result<CommandResponse> {
        let guild_id = interaction
            .guild_id
            .ok_or_else(|| anyhow!("Must be run in a guild"))?;
        if let Some(cmd) = guild_id
            .get_application_commands(&ctx.http)
            .await?
            .iter()
            .find(|cmd| cmd.name == self.command_name)
        {
            guild_id
                .delete_application_command(&ctx.http, cmd.id)
                .await?;
        }
        let db = handler.db.lock().await;
        db.execute(
            "DELETE FROM forms WHERE guild_id = ?1 AND command_name = ?2",
            params![guild_id.0, &self.command_name],
        )?;
        {
            let mut forms = handler.forms.write().await;
            forms.retain(|form| form.command_name != self.command_name);
        }
        Ok(CommandResponse::Public(format!(
            "Deleted command {}",
            &self.command_name
        )))
    }
}

#[derive(Command, Debug)]
#[cmd(name = "list_forms", desc = "List submission forms and commands")]
pub struct ListForms {}

#[async_trait]
impl BotCommand for ListForms {
    type Data = Handler;

    async fn run(
        self,
        handler: &Handler,
        _ctx: &Context,
        interaction: &ApplicationCommandInteraction,
    ) -> anyhow::Result<CommandResponse> {
        let guild_id = interaction
            .guild_id
            .ok_or_else(|| anyhow!("Must be run in a guild"))?
            .0;
        let forms = handler.forms.read().await;
        let contents = forms
            .iter()
            .filter(|form| form.guild_id == guild_id)
            .map(|form| {
                format!(
                    "**· [{}]({}):** </{}:{}>",
                    &form.form.title, &form.form.responder_uri, &form.command_name, form.command_id,
                )
            })
            .join("\n");
        let mut embed = CreateEmbed::default();
        embed.title("Registered forms").description(contents);
        Ok(CommandResponse::Embed(embed))
    }
}

pub fn load_forms(db: &Connection) -> anyhow::Result<Vec<FormCommand>> {
    let mut stmt =
        db.prepare("SELECT guild_id, command_name, command_id, form, submission_type FROM forms")?;
    let commands = stmt
        .query([])?
        .map(|row| {
            Ok(FormCommand {
                guild_id: row.get(0)?,
                command_name: row.get(1)?,
                command_id: row.get(2)?,
                form: serde_json::from_slice(row.get::<_, String>(3)?.as_bytes()).unwrap(),
                submission_type: row.get(4)?,
            })
        })
        .collect::<Vec<_>>()?;
    Ok(commands)
}

impl SimpleForm {
    pub fn responder_id(&self) -> &str {
        self.responder_uri
            .trim_start_matches("https://docs.google.com/forms/d/e/")
            .trim_end_matches("/viewform")
    }

    pub fn form_response_url(&self) -> String {
        format!(
            "https://docs.google.com/forms/u/0/d/e/{}/formResponse",
            self.responder_id()
        )
    }

    pub async fn submit(
        &self,
        handler: &Handler,
        _ctx: &Context,
        interaction: &ApplicationCommandInteraction,
        submission_type: &str,
    ) -> anyhow::Result<CommandResponse> {
        let user = &interaction.user;
        let user_handle = format!("{}#{:04}", &user.name, user.discriminator);
        let mut values = vec!["".to_string(); self.questions.len() + 1];
        let now = Local::now();
        let timestamp = format!("{}", now.format("%m/%d/%Y %H:%M:%S"));
        // let song = handler.spotify.get_song_from_url(&self.link).await?;
        values[0].push_str(&timestamp);
        values[1].push_str(&user_handle);

        let mut song_infos = Vec::new();
        let mut song_urls = Vec::new();
        let mut value_pairs = Vec::with_capacity(self.questions.len());
        let mut next_value = None;
        for q in self.questions.iter().rev() {
            let question_id = u64::from_str_radix(&q.id, 16).context("Invalid form definition")?;
            if q.title.to_lowercase().contains("user") {
                value_pairs.push((question_id, user_handle.clone()));
                continue;
            }
            let sanitized = sanitize_name(&q.title);
            let value = interaction
                .data
                .options
                .iter()
                .find(|opt| opt.name == sanitized)
                .and_then(|opt| match &opt.resolved {
                    Some(CommandDataOptionValue::String(s)) => Some(s.clone()),
                    _ => None,
                })
                .or_else(|| next_value.take());
            let mut value = match value {
                Some(v) => v,
                None => continue,
            };
            if sanitized.contains("spotify") || sanitized.contains("link") {
                if submission_type == "album" {
                    if let Some(p) = handler.providers.iter().find(|p| p.url_matches(&value)) {
                        let album = p.get_from_url(&value).await?;
                        let album_info = album.format_name();
                        next_value = Some(album_info.clone());
                        value = album.url.clone();
                        song_infos.push(album_info)
                    }
                } else {
                    let song = handler.spotify.get_song_from_url(&value).await?;
                    if song.duration > Duration::from_secs(60 * 20) {
                        bail!("This song is too long!")
                    }
                    let song_info = format!(
                        "{} - {}",
                        spotify::artists_to_string(&song.artists),
                        &song.name,
                    );
                    next_value = Some(song_info.clone());
                    value = song.id.unwrap().url();
                    song_infos.push(song_info);
                    song_urls.push(value.to_string());
                }
            }
            value_pairs.push((question_id, value));
        }
        let form_data = value_pairs
            .into_iter()
            .map(|(id, value)| format!("entry.{id}={}", urlencoding::encode(&value)))
            .join("&");

        let url = self.form_response_url();
        let req = Request::builder()
            .uri(url)
            .method(Method::POST)
            .header("Content-Type", "application/x-www-form-urlencoded")
            .body(Body::from(form_data.into_bytes()))?;
        let resp = handler.forms_client.client.request(req).await?;
        if resp.status() != StatusCode::OK {
            bail!("Failed to send response: status {}", resp.status());
        }

        let contents = if !song_infos.is_empty() {
            format!(
                "Submitted {} to {}\n{}",
                song_infos.join(", "),
                &self.title,
                song_urls.join("\n")
            )
        } else {
            format!("Submitted to {}", &self.title)
        };
        Ok(CommandResponse::Private(contents))
    }
}
