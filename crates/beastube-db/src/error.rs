//! Storage errors and their recovery strategies.

use beastube_core::error::{DomainError, ErrorKind, Recovery};

/// A storage-layer failure.
#[derive(Debug, thiserror::Error)]
pub enum DbError {
    /// The database file could not be opened or created.
    #[error("could not open the database at {path}")]
    Open {
        /// Path that was attempted.
        path: String,
        /// Underlying driver error.
        #[source]
        source: sqlx::Error,
    },

    /// A migration failed to apply.
    ///
    /// Distinct from [`DbError::Query`] because it is unrecoverable at runtime: the schema is not
    /// in a state the code can use, so the application must not proceed to normal operation.
    #[error("database migration failed")]
    Migration(#[from] sqlx::migrate::MigrateError),

    /// A statement failed.
    #[error("database query failed")]
    Query(#[from] sqlx::Error),

    /// The write lock could not be acquired within the busy timeout.
    ///
    /// Separated from [`DbError::Query`] because it is transient and retryable, whereas most query
    /// failures are not.
    #[error("the database was locked by another write")]
    Locked {
        /// Underlying driver error.
        #[source]
        source: sqlx::Error,
    },

    /// An integrity check failed, or a row could not be decoded into its domain type.
    #[error("database integrity problem: {detail}")]
    Corrupt {
        /// What was found to be wrong.
        detail: String,
    },

    /// A stored value could not be parsed back into its domain type.
    ///
    /// Treated as corruption of that row rather than of the database: the row is rebuildable, so
    /// recovery discards it instead of quarantining the file.
    #[error("stored value in {table}.{column} could not be decoded")]
    Decode {
        /// Table the row came from.
        table: &'static str,
        /// Column that failed to decode.
        column: &'static str,
        /// Underlying parse error.
        #[source]
        source: serde_json::Error,
    },

    /// A referenced row does not exist.
    #[error("{entity} `{id}` was not found")]
    NotFound {
        /// Kind of row that was missing.
        entity: &'static str,
        /// Identifier that was looked up.
        id: String,
    },

    /// A caller-supplied value violated a domain invariant before reaching SQL.
    #[error("invalid {field}: {reason}")]
    Invalid {
        /// Field that was rejected.
        field: &'static str,
        /// Why it was rejected.
        reason: String,
    },
}

impl DbError {
    /// Classifies a driver error, promoting SQLite busy/locked into [`DbError::Locked`] and
    /// on-disk corruption into [`DbError::Corrupt`].
    ///
    /// Callers use this instead of `?` on `sqlx::Error` wherever the distinction matters, because
    /// "retry in a moment" and "the file is damaged" call for opposite responses.
    #[must_use]
    pub fn from_sqlx(source: sqlx::Error) -> Self {
        if let sqlx::Error::Database(db) = &source {
            // SQLite result codes: 5 = SQLITE_BUSY, 6 = SQLITE_LOCKED, 11 = SQLITE_CORRUPT,
            // 26 = SQLITE_NOTADB. Extended codes share the low byte, so compare the primary code.
            let primary = db
                .code()
                .and_then(|c| c.parse::<i32>().ok())
                .map(|c| c & 0xff);
            match primary {
                Some(5 | 6) => return Self::Locked { source },
                Some(11 | 26) => {
                    return Self::Corrupt {
                        detail: source.to_string(),
                    };
                }
                _ => {}
            }
        }
        Self::Query(source)
    }

    /// Whether retrying the same operation could succeed.
    #[must_use]
    pub const fn is_transient(&self) -> bool {
        matches!(self, Self::Locked { .. })
    }
}

impl DomainError for DbError {
    fn kind(&self) -> ErrorKind {
        ErrorKind::Database
    }

    fn code(&self) -> &'static str {
        match self {
            Self::Open { .. } => "open_failed",
            Self::Migration(_) => "migration_failed",
            Self::Query(_) => "query_failed",
            Self::Locked { .. } => "locked",
            Self::Corrupt { .. } => "corrupt",
            Self::Decode { .. } => "decode_failed",
            Self::NotFound { .. } => "not_found",
            Self::Invalid { .. } => "invalid_input",
        }
    }

    // Variants sharing a recovery today are still distinct failures; merging the arms would couple rules that are expected to diverge.
    #[allow(clippy::match_same_arms)]
    fn recovery(&self) -> Recovery {
        match self {
            // Contention clears on its own; the busy timeout has already waited once, so back off
            // briefly rather than immediately.
            Self::Locked { .. } => Recovery::RetryAutomatic {
                delay_ms: 250,
                attempts_made: 1,
                max_attempts: 5,
            },
            // A single undecodable row is discarded and refetched; the database is fine.
            Self::Decode { .. } => Recovery::RebuildLocalData {
                store: "cache.metadata".to_owned(),
            },
            // Genuine file corruption. The recovery path quarantines the file and rebuilds, which
            // loses local library data — so it is surfaced to the user rather than done silently.
            Self::Corrupt { .. } => Recovery::RebuildLocalData {
                store: "database".to_owned(),
            },
            // A failed migration leaves an unusable schema; continuing would corrupt data.
            Self::Migration(_) | Self::Open { .. } => Recovery::Unrecoverable,
            Self::NotFound { .. } | Self::Invalid { .. } => Recovery::Unrecoverable,
            Self::Query(_) => Recovery::RetryManual,
        }
    }

    fn params(&self) -> std::collections::BTreeMap<String, String> {
        let mut params = std::collections::BTreeMap::new();
        match self {
            Self::NotFound { entity, id } => {
                params.insert("entity".to_owned(), (*entity).to_owned());
                params.insert("id".to_owned(), id.clone());
            }
            Self::Invalid { field, reason } => {
                params.insert("field".to_owned(), (*field).to_owned());
                params.insert("reason".to_owned(), reason.clone());
            }
            Self::Open { path, .. } => {
                params.insert("path".to_owned(), path.clone());
            }
            _ => {}
        }
        params
    }
}

/// Convenience alias for storage results.
pub type DbResult<T> = Result<T, DbError>;

#[cfg(test)]
mod tests {
    use super::*;
    use beastube_core::error::ErrorKind;

    #[test]
    fn errors_classify_as_database_failures() {
        let err = DbError::NotFound {
            entity: "playlist",
            id: "7".to_owned(),
        };
        assert_eq!(err.kind(), ErrorKind::Database);
        assert_eq!(err.full_code(), "database.not_found");
        assert_eq!(err.message_key(), "error.database.not_found");
    }

    #[test]
    fn only_lock_contention_is_transient() {
        assert!(
            DbError::Locked {
                source: sqlx::Error::PoolClosed
            }
            .is_transient()
        );
        assert!(
            !DbError::Corrupt {
                detail: "malformed".to_owned()
            }
            .is_transient()
        );
        assert!(
            !DbError::NotFound {
                entity: "video",
                id: "x".to_owned()
            }
            .is_transient()
        );
    }

    #[test]
    fn a_locked_database_retries_automatically_but_finitely() {
        let recovery = DbError::Locked {
            source: sqlx::Error::PoolClosed,
        }
        .recovery();
        match recovery {
            Recovery::RetryAutomatic { max_attempts, .. } => {
                assert!(
                    max_attempts > 0 && max_attempts <= 10,
                    "retries must be bounded"
                );
            }
            other => panic!("expected an automatic retry, got {other:?}"),
        }
    }

    #[test]
    fn migration_failure_is_never_retried() {
        assert_eq!(
            DbError::Migration(sqlx::migrate::MigrateError::VersionMissing(1)).recovery(),
            Recovery::Unrecoverable,
            "an unusable schema must stop startup, not spin"
        );
    }

    #[test]
    fn a_bad_row_rebuilds_the_cache_not_the_database() {
        let err = DbError::Decode {
            table: "videos",
            column: "details_json",
            source: serde_json::from_str::<i32>("{").unwrap_err(),
        };
        assert_eq!(
            err.recovery(),
            Recovery::RebuildLocalData {
                store: "cache.metadata".to_owned()
            },
            "one undecodable row must not escalate to discarding the user's library"
        );
    }

    #[test]
    fn payloads_carry_identifying_params_for_the_ui() {
        let payload = DbError::NotFound {
            entity: "playlist",
            id: "42".to_owned(),
        }
        .to_payload();
        assert_eq!(
            payload.params.get("entity").map(String::as_str),
            Some("playlist")
        );
        assert_eq!(payload.params.get("id").map(String::as_str), Some("42"));
        assert!(payload.diagnostic.is_some());
    }
}
