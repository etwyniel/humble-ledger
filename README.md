# Getting started

First you need to grab API credentials:

### Google sheets api

* See https://developers.google.com/sheets/api/quickstart/js
  * Enable Sheets API
  * Create service account credentials
    * Api & Services
    * -> Credentials
    * -> Create Credentials
    * -> Service Account
  * Save json to `credentials.json`


### Create a discord API token
* Create an application: https://discord.com/developers/applications
  * New Application, fill in name (your choice)
  * Under OAuth2 -> URL generator
  * Enable `bot`, select permissions
    * This seems(?) to be the required set:
    * Privileged Gateway Intents:
      * Presence Intent
      * Message Content Intent
    * Read Messages/view Channels
    * Send Messages
    * Add Reactions
  * Permission Integer: 3136
  * Save URL (needed to add bot to server)
* Save the application id
* Under bot tab, create a token and save it

### Spotify API credentials

* https://developer.spotify.com/dashboard
* Create an app
* Set redirect uri to something that doesn't exist ("http://localhost:9999/")
  * Save the redirect uri verbatim (copy/paste)
* Got to settings, save the client ID

### Last.fm API account

* Create a last.fm API account: https://www.last.fm/api/account/create
* Save the API_KEY

Required environment variables:
* `DISCORD_TOKEN` - From `Create a discord API token`
* `APPLICATION_ID`- From `Create a discord API token`
* `RSPOTIFY_CLIENT_ID` - From `Spotify API credentials`
* `RSPOTIFY_CLIENT_SECRET` - From `Spotify API credentials`
* `RSPOTIFY_REDIRECT_URI` -  From `Spotify API credentials`
* `LFM_API_KEY` - From `Last.fm API account`

# Build

## Requirements

* rust and cargo
* clang
* openssl-dev
* sqlite
* pkgconfig

* make (optional)

# How to build

* Run `make`
* Alternatively: `cargo build`

# Running the bot

* Make sure `credentials.json` exists
* Write environment variables to `bot.env`:
```sh
APPLICATION_ID=<...>
DISCORD_PUBLIC_KEY=<...>
DISCORD_TOKEN=<...>
RSPOTIFY_CLIENT_ID=<...>
RSPOTIFY_CLIENT_SECRET=<...>
RSPOTIFY_REDIRECT_URI=<...>
LFM_API_KEY=<...>
```

* Run `make run` or `./run.sh`
