use fallible_iterator::FallibleIterator;
use rusqlite::{params, Connection};

use crate::{playlist::Playlist, Handler};

impl Handler {
    pub async fn get_playlist(
        &self,
        guild_id: u64,
        playlist_command_name: &str,
    ) -> anyhow::Result<Playlist> {
        let db = self.db.lock().await;
        let playlist = db.query_row(
            "SELECT name, spreadsheet_id, has_backup FROM playlists WHERE guild_id = ?1 AND command_name = ?2",
            params![guild_id, playlist_command_name],
                     |row| Ok(Playlist {
                         name: row.get(0)?,
                         spreadsheet_id: row.get(1)?,
                         has_backup: row.get(2)?,
                     }))?;
        Ok(playlist)
    }

    pub async fn save_playlist(&self, guild_id: u64, playlist: &Playlist) -> anyhow::Result<()> {
        let db = self.db.lock().await;
        db.execute("INSERT INTO playlists (guild_id, name, command_name, spreadsheet_id, has_backup) VALUES (?1, ?2, ?3, ?4, ?5)",
        params![guild_id, &playlist.name, playlist.command_name(), playlist.spreadsheet_id, playlist.has_backup])?;
        Ok(())
    }

    pub async fn delete_playlist(
        &self,
        guild_id: u64,
        playlist_command_name: &str,
    ) -> anyhow::Result<()> {
        let db = self.db.lock().await;
        db.execute(
            "DELETE FROM playlists WHERE guild_id = ?1 AND command_name = ?2",
            params![guild_id, playlist_command_name],
        )?;
        Ok(())
    }

    pub async fn list_playlists(&self, guild_id: u64) -> anyhow::Result<Vec<Playlist>> {
        let db = self.db.lock().await;
        let mut stmt = db.prepare(
            "SELECT name, spreadsheet_id, has_backup FROM playlists WHERE guild_id = ?1",
        )?;
        let res = stmt
            .query([guild_id])?
            .map(|row| {
                Ok(Playlist {
                    name: row.get(0)?,
                    spreadsheet_id: row.get(1)?,
                    has_backup: row.get(2)?,
                })
            })
            .collect()
            .map_err(anyhow::Error::from);
        res
    }
}

pub fn init() -> anyhow::Result<Connection> {
    let conn = Connection::open("humble_ledger.sqlite")?;
    conn.execute(
        "CREATE TABLE IF NOT EXISTS playlists (
            guild_id INTEGER NOT NULL,
            command_name STRING NOT NULL,
            name STRING NOT NULL,
            spreadsheet_id STRING NOT NULL,
            has_backup BOOLEAN NOT NULL DEFAULT(FALSE),

            UNIQUE(guild_id, command_name)
        )",
        [],
    )?;
    conn.execute(
        "CREATE TABLE IF NOT EXISTS forms (
            guild_id INTEGER NOT NULL,
            command_name STRING NOT NULL,
            command_id INTEGER NOT NULL,
            form STRING NOT NULL,
            submission_type STRING NOT NULL DEFAULT('song'),

            UNIQUE(guild_id, command_name)
        )",
        [],
    )?;
    Ok(conn)
}
