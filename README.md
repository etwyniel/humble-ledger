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
  * New Application, fill in name
  * Under OAuth2 -> URL generator
  * Enable `bot`, select permissions (TODO: document permission set)
  * Save URL (needed to add bot to server)
* Save the application id
* Under bot tab, create a token and save it

### Spotify API credentials

* https://developer.spotify.com/dashboard
* Create an app
* Get client ID and secret


Required environment variables:
* `DISCORD_TOKEN` - From `Create a discord API token`
* `APPLICATION_ID`- From `Create a discord API token`
* `RSPOTIFY_CLIENT_ID` - From `Spotify API credentials`
* `RSPOTIFY_CLIENT_SECRET` - From `Spotify API credentials`

# Requirements

* rust and cargo
* clang
* openssl-dev
* sqlite
* pkgconfig

* make (optional)

# Building

* Make sure submodules are updated: `git submodule update --init`
* Run `make`

# Running the bot

* Make sure `credentials.json` exists
* Write environment variables to `bot.env`:
```sh
APPLICATION_ID=<...>
DISCORD_PUBLIC_KEY=<...>
DISCORD_TOKEN=<...>
RSPOTIFY_CLIENT_ID=<...>
RSPOTIFY_CLIENT_SECRET=<...>

```

* Run `make run` or `./run.sh`
