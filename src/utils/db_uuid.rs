use std::fmt;
use std::ops::Deref;

use sqlx::Sqlite;
use sqlx::database::Database;
use sqlx::encode::IsNull;
use sqlx::error::BoxDynError;
use uuid::Uuid;

/// A `Uuid` stored as `TEXT` (canonical `8-4-4-4-12` form) rather than sqlx's
/// default `BLOB` mapping for SQLite — chosen so ids stay human-readable when
/// inspecting the database directly (sqlite3 CLI, Turso console).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(transparent)]
pub struct DbUuid(pub Uuid);

impl From<Uuid> for DbUuid {
    fn from(id: Uuid) -> Self {
        DbUuid(id)
    }
}

impl From<DbUuid> for Uuid {
    fn from(id: DbUuid) -> Self {
        id.0
    }
}

impl Deref for DbUuid {
    type Target = Uuid;

    fn deref(&self) -> &Uuid {
        &self.0
    }
}

impl fmt::Display for DbUuid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl sqlx::Type<Sqlite> for DbUuid {
    fn type_info() -> <Sqlite as Database>::TypeInfo {
        <String as sqlx::Type<Sqlite>>::type_info()
    }

    fn compatible(ty: &<Sqlite as Database>::TypeInfo) -> bool {
        <String as sqlx::Type<Sqlite>>::compatible(ty)
    }
}

impl<'q> sqlx::Encode<'q, Sqlite> for DbUuid {
    fn encode_by_ref(
        &self,
        buf: &mut <Sqlite as Database>::ArgumentBuffer,
    ) -> Result<IsNull, BoxDynError> {
        <String as sqlx::Encode<'q, Sqlite>>::encode(self.0.to_string(), buf)
    }
}

impl<'r> sqlx::Decode<'r, Sqlite> for DbUuid {
    fn decode(value: <Sqlite as Database>::ValueRef<'r>) -> Result<Self, BoxDynError> {
        let s = <String as sqlx::Decode<'r, Sqlite>>::decode(value)?;
        Ok(DbUuid(Uuid::parse_str(&s)?))
    }
}
