#!/usr/bin/env bash

set -e

source bot.env

fail () {
  echo "$@" >&2
  exit 1
}

required_variables=( APPLICATION_ID\
                     DISCORD_TOKEN\
                     RSPOTIFY_CLIENT_ID\
                     RSPOTIFY_CLIENT_SECRET\
                     RSPOTIFY_REDIRECT_URI\
                     LFM_API_KEY\
          )

# Check that variables are set and export
for v in "${required_variables[@]}"; do
  [[ -n ${!v} ]] || fail "Variable $v is unset"
  export "${v?}"
done
[[ -f credentials.json ]] || fail "credentials.json missing"


exec dist/humble_ledger "$@"
