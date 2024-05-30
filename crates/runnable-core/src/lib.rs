use std::path::Path;

use bstr::ByteSlice as _;

const MARKER: &[u8; 32] = b"brioche_runnable_v0             ";

const LENGTH_BYTES: usize = 4;
type LengthInt = u32;

#[derive(Debug, serde::Serialize, serde::Deserialize, bincode::Encode, bincode::Decode)]
pub struct Runnable {
    pub command: RunnableTemplate,
    pub args: Vec<RunnableTemplate>,
    pub env: Vec<(String, RunnableTemplate)>,
}

#[derive(Debug, serde::Serialize, serde::Deserialize, bincode::Encode, bincode::Decode)]
pub struct RunnableTemplate {
    components: Vec<RunnableTemplateComponent>,
}

impl RunnableTemplate {
    pub fn to_os_string(&self, base: &Path) -> Result<std::ffi::OsString, RunnableTemplateError> {
        let mut os_string = std::ffi::OsString::new();

        for component in &self.components {
            match component {
                RunnableTemplateComponent::Literal { value } => {
                    let value = value.to_os_str()?;
                    os_string.push(value);
                }
                RunnableTemplateComponent::RelativePath { path } => {
                    let path = path.to_path()?;
                    let path = base.join(path);
                    os_string.push(path);
                }
            }
        }

        Ok(os_string)
    }
}

#[derive(Debug, serde::Serialize, serde::Deserialize, bincode::Encode, bincode::Decode)]
pub enum RunnableTemplateComponent {
    Literal { value: Vec<u8> },
    RelativePath { path: Vec<u8> },
}

pub fn inject(
    mut writer: impl std::io::Write,
    runnable: &Runnable,
) -> Result<(), InjectRunnableError> {
    let runnable_bytes = bincode::encode_to_vec(runnable, bincode::config::standard())
        .map_err(InjectRunnableError::SerializeError)?;
    let runnable_length: LengthInt = runnable_bytes
        .len()
        .try_into()
        .map_err(|_| InjectRunnableError::PackTooLarge)?;
    let length_bytes = runnable_length.to_le_bytes();

    writer.write_all(MARKER)?;
    writer.write_all(&length_bytes)?;
    writer.write_all(&runnable_bytes)?;
    writer.write_all(&length_bytes)?;
    writer.write_all(MARKER)?;

    Ok(())
}

pub fn extract(mut reader: impl std::io::Read) -> Result<Runnable, ExtractRunnableError> {
    let mut program = vec![];
    reader
        .read_to_end(&mut program)
        .map_err(ExtractRunnableError::ReadPackedProgramError)?;

    let program = program
        .strip_suffix(MARKER)
        .ok_or_else(|| ExtractRunnableError::MarkerNotFound)?;
    let (program, length_bytes) = program.split_at(program.len().wrapping_sub(LENGTH_BYTES));
    let length_bytes: [u8; LENGTH_BYTES] = length_bytes
        .try_into()
        .map_err(|_| ExtractRunnableError::MalformedMarker)?;
    let length = LengthInt::from_le_bytes(length_bytes);
    let length: usize = length
        .try_into()
        .map_err(|_| ExtractRunnableError::MalformedMarker)?;

    let (program, runnable) = program.split_at(program.len().wrapping_sub(length));
    let program = program
        .strip_suffix(&length_bytes)
        .ok_or_else(|| ExtractRunnableError::MalformedMarker)?;
    let _program = program
        .strip_suffix(MARKER)
        .ok_or_else(|| ExtractRunnableError::MalformedMarker)?;

    let (runnable, _) = bincode::decode_from_slice(runnable, bincode::config::standard())
        .map_err(ExtractRunnableError::InvalidPack)?;

    Ok(runnable)
}

#[derive(Debug, thiserror::Error)]
pub enum InjectRunnableError {
    #[error("failed to write runnable program: {0}")]
    IoError(#[from] std::io::Error),
    #[error("failed to serialize runnable data: {0}")]
    SerializeError(#[source] bincode::error::EncodeError),
    #[error("runnable data too large")]
    PackTooLarge,
}

#[derive(Debug, thiserror::Error)]
pub enum ExtractRunnableError {
    #[error("failed to read runnable data: {0}")]
    ReadPackedProgramError(#[source] std::io::Error),
    #[error("marker not found at end of the runnable program")]
    MarkerNotFound,
    #[error("marker was malformed at the end of the runnable program")]
    MalformedMarker,
    #[error("failed to parse runnable: {0}")]
    InvalidPack(#[source] bincode::error::DecodeError),
}

#[derive(Debug, thiserror::Error)]
pub enum RunnableTemplateError {
    #[error("invalid UTF-8 in runnable template: {0}")]
    Utf8Error(#[from] bstr::Utf8Error),
}
