use std::{borrow::Cow, ffi::OsStr, fs, path::Path};

use thiserror::Error;

pub fn from_os_str(os_value: &OsStr) -> Result<Cow<'_, str>, Error> {
    let bytes = os_value.as_encoded_bytes();
    if bytes.starts_with(b"file:") {
        let ref_file = fs::read(Path::new(unsafe {
            OsStr::from_encoded_bytes_unchecked(&bytes[5..])
        }))?;
        String::from_utf8(ref_file)
            .map_err(|_| Error::InvalidUtf8)
            .map(Cow::Owned)
    } else {
        os_value
            .to_str()
            .ok_or(Error::InvalidUtf8)
            .map(Cow::Borrowed)
    }
}

#[derive(Debug, Error)]
pub enum Error {
    #[error("IO error: {0}")]
    IO(#[from] std::io::Error),
    #[error("invalid utf8")]
    InvalidUtf8,
}
