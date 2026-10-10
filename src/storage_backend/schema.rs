//! The schema versions a database records, and what opening one needs.
//!
//! Both backends record the version in `_config` and read it back on open.
//! Which of the open-time migrations a recorded version calls for is decided
//! here, in one pure function, so the decision is the same on both backends
//! and can be tested without a database.

use super::sql_builders::SCHEMA_VERSION;

/// The version that first keys a multi-attribute GSI on every key attribute.
/// A database recorded below it may hold such an index keyed on the first
/// attribute of each kind, and its multi-attribute GSIs are rebuilt on open.
pub const MULTI_ATTRIBUTE_KEYS: u32 = 10;

/// The version that first stores the items of a table with a vector index at
/// their billed size. A database recorded below it holds number-rule sizes
/// for those items, and they are recomputed on open.
pub const VECTOR_BILLED_SIZES: u32 = 11;

// The version this build writes is the version its last migration records.
const _: () = assert!(SCHEMA_VERSION == VECTOR_BILLED_SIZES);

/// What opening a database at a recorded schema version needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OpenMigrations {
    /// Rebuild every multi-attribute GSI from its base table.
    pub rebuild_multi_attribute_gsis: bool,
    /// Recompute the stored item sizes of every table with a vector index.
    pub recompute_vector_sizes: bool,
    /// The version a newer build than this one recorded, when one did.
    /// Nothing is migrated, and the open refuses the file.
    pub newer_than_this_build: Option<u32>,
}

/// The open-time migrations a database recorded at `recorded` needs.
///
/// `None` is a database with no recorded version: one the wasm backend wrote
/// before it began recording a version, or one this open is creating. Both
/// migrations run for it; on a new database there is nothing for either to
/// do.
pub fn open_migrations(recorded: Option<u32>) -> OpenMigrations {
    match recorded {
        None => OpenMigrations {
            rebuild_multi_attribute_gsis: true,
            recompute_vector_sizes: true,
            newer_than_this_build: None,
        },
        Some(version) => OpenMigrations {
            rebuild_multi_attribute_gsis: version < MULTI_ATTRIBUTE_KEYS,
            recompute_vector_sizes: version < VECTOR_BILLED_SIZES,
            newer_than_this_build: (version > SCHEMA_VERSION).then_some(version),
        },
    }
}

/// Refuse a database a newer build wrote: one whose recorded version is past
/// [`SCHEMA_VERSION`]. This build can't know what that build changed, so it
/// neither reads nor migrates such a file. Both backends check this before an
/// open writes anything, so a refused file is left as it was found.
pub fn refuse_newer(recorded: Option<u32>) -> Result<(), String> {
    match recorded {
        Some(version) if version > SCHEMA_VERSION => Err(newer_than_this_build(version)),
        _ => Ok(()),
    }
}

/// What an open says of a database a newer build wrote at `version`.
pub fn newer_than_this_build(version: u32) -> String {
    format!(
        "this database was written by a newer version of Dynoxide (schema version \
         {version}; this build reads up to schema version {SCHEMA_VERSION}). Upgrade \
         Dynoxide to open it. The file has not been changed."
    )
}

/// Read a recorded schema version. A value that isn't a number reads as no
/// version, so the open runs every migration rather than skipping them.
pub fn parse_recorded(value: Option<&str>) -> Option<u32> {
    value.and_then(|v| v.trim().parse().ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    const BOTH: OpenMigrations = OpenMigrations {
        rebuild_multi_attribute_gsis: true,
        recompute_vector_sizes: true,
        newer_than_this_build: None,
    };

    #[test]
    fn an_unversioned_database_runs_every_migration() {
        assert_eq!(open_migrations(None), BOTH);
    }

    #[test]
    fn a_database_before_multi_attribute_keys_runs_both() {
        for version in 1..MULTI_ATTRIBUTE_KEYS {
            assert_eq!(open_migrations(Some(version)), BOTH, "version {version}");
        }
    }

    #[test]
    fn a_database_at_multi_attribute_keys_only_recomputes_sizes() {
        assert_eq!(
            open_migrations(Some(MULTI_ATTRIBUTE_KEYS)),
            OpenMigrations {
                rebuild_multi_attribute_gsis: false,
                recompute_vector_sizes: true,
                newer_than_this_build: None,
            }
        );
    }

    #[test]
    fn a_database_at_this_version_runs_nothing() {
        assert_eq!(
            open_migrations(Some(SCHEMA_VERSION)),
            OpenMigrations {
                rebuild_multi_attribute_gsis: false,
                recompute_vector_sizes: false,
                newer_than_this_build: None,
            }
        );
    }

    #[test]
    fn a_database_from_a_newer_build_runs_nothing_and_says_so() {
        assert_eq!(
            open_migrations(Some(SCHEMA_VERSION + 1)),
            OpenMigrations {
                rebuild_multi_attribute_gsis: false,
                recompute_vector_sizes: false,
                newer_than_this_build: Some(SCHEMA_VERSION + 1),
            }
        );
    }

    #[test]
    fn only_a_version_past_this_one_is_refused() {
        for recorded in [None, Some(1), Some(SCHEMA_VERSION)] {
            assert_eq!(refuse_newer(recorded), Ok(()), "{recorded:?}");
        }
        let message = refuse_newer(Some(SCHEMA_VERSION + 1)).unwrap_err();
        assert_eq!(
            message,
            format!(
                "this database was written by a newer version of Dynoxide (schema version \
                 {}; this build reads up to schema version {SCHEMA_VERSION}). Upgrade \
                 Dynoxide to open it. The file has not been changed.",
                SCHEMA_VERSION + 1
            )
        );
    }

    #[test]
    fn a_recorded_version_that_is_not_a_number_reads_as_none() {
        assert_eq!(parse_recorded(Some("11")), Some(11));
        assert_eq!(parse_recorded(Some("eleven")), None);
        assert_eq!(parse_recorded(None), None);
    }
}
