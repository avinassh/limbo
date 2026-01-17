use crate::helpers::wrapper::Wrapper;

pub use super::connection::RustConnection;
use std::sync::Arc;

pub enum OpenFlags {
    ReadOnly,
    ReadWrite,
    Create,
}

pub struct ConnectArgs {
    pub url: String,
    pub auth_token: Option<String>,
    pub sync_url: Option<String>,
    pub sync_interval_seconds: Option<u64>,
    pub encryption_key: Option<String>,
    pub encryption_cipher: Option<String>,
    pub read_your_writes: Option<bool>,
    pub open_flags: Option<OpenFlags>,
    pub offline: Option<bool>,
}

pub async fn connect(args: ConnectArgs) -> RustConnection {
    // Prepare encryption options if both cipher and key are provided
    let encryption_opts = match (args.encryption_cipher, args.encryption_key) {
        (Some(cipher), Some(hexkey)) => Some(turso_core::EncryptionOpts { cipher, hexkey }),
        _ => None,
    };

    let database = if args.url == ":memory:" {
        let io: Arc<dyn turso_core::IO> = Arc::new(turso_core::MemoryIO::new());
        turso_core::Database::open_file_with_flags(
            io,
            args.url.as_str(),
            turso_core::OpenFlags::default(),
            turso_core::DatabaseOpts::new(),
            encryption_opts,
        )
    } else {
        let io: Arc<dyn turso_core::IO> = Arc::new(turso_core::PlatformIO::new().unwrap());
        turso_core::Database::open_file_with_flags(
            io,
            args.url.as_str(),
            turso_core::OpenFlags::default(),
            turso_core::DatabaseOpts::new(),
            encryption_opts,
        )
    }
    .unwrap();
    let connection = database.connect().unwrap();
    RustConnection::new(Wrapper { inner: connection })
}
