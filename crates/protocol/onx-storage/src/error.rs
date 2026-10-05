use onx_state_model::StateModelError;
use onx_stf::StfError;
use std::{fmt, io};

/// Errors from chain storage.
///
/// Every variant is fail-closed: corruption, schema drift, and forks are
/// reported, never silently absorbed.
#[derive(Debug)]
pub enum StorageError {
    /// Filesystem I/O failure (creating directories, etc.).
    Io(io::Error),
    /// redb backend failure (boxed: the type is large and errors are rare-path).
    Redb(Box<redb::Error>),
    /// Persisted data is structurally invalid or internally inconsistent.
    Corrupt(String),
    /// The database was written by a newer schema version than this binary
    /// understands. Refusing to open is fail-closed: misinterpreting a
    /// newer layout would be silent corruption.
    SchemaMismatch { found: u32, supported: u32 },
    /// A different block hash is already committed at this sequence number.
    /// The chain forked under us; this must never happen silently.
    ForkDetected {
        seqno: u32,
        existing: [u8; 32],
        incoming: [u8; 32],
    },
    /// State-model failure (decoding a stored record, trie construction).
    State(StateModelError),
    /// The STF rejected the block being committed.
    Stf(StfError),
}

impl fmt::Display for StorageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(e) => write!(f, "storage I/O error: {e}"),
            Self::Redb(e) => write!(f, "storage backend error: {e}"),
            Self::Corrupt(msg) => write!(f, "corrupt persisted state: {msg}"),
            Self::SchemaMismatch { found, supported } => write!(
                f,
                "schema version {found} is newer than supported {supported}; refusing to open"
            ),
            Self::ForkDetected {
                seqno,
                existing,
                incoming,
            } => write!(
                f,
                "fork detected at seqno {seqno}: stored {}, incoming {}",
                hex16(existing),
                hex16(incoming)
            ),
            Self::State(e) => write!(f, "state error: {e}"),
            Self::Stf(e) => write!(f, "block rejected by STF: {e}"),
        }
    }
}

impl std::error::Error for StorageError {}

impl From<io::Error> for StorageError {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}

impl From<redb::Error> for StorageError {
    fn from(e: redb::Error) -> Self {
        Self::Redb(Box::new(e))
    }
}

// redb surfaces granular error types; they all fold into `redb::Error`.
macro_rules! from_redb_error {
    ($($t:ty),*) => {
        $(
            impl From<$t> for StorageError {
                fn from(e: $t) -> Self {
                    Self::Redb(Box::new(redb::Error::from(e)))
                }
            }
        )*
    };
}
from_redb_error!(
    redb::DatabaseError,
    redb::TransactionError,
    redb::TableError,
    redb::StorageError,
    redb::CommitError
);

impl From<StateModelError> for StorageError {
    fn from(e: StateModelError) -> Self {
        Self::State(e)
    }
}

impl From<StfError> for StorageError {
    fn from(e: StfError) -> Self {
        Self::Stf(e)
    }
}

fn hex16(h: &[u8; 32]) -> String {
    h.iter().take(8).map(|b| format!("{b:02x}")).collect()
}
