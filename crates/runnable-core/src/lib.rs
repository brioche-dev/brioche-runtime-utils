use std::path::{Path, PathBuf};

use bstr::ByteSlice as _;
use encoding::TickEncoded;

mod encoding;

pub const FORMAT: &str = "application/vnd.brioche.runnable-v0.1.0+json";

#[serde_with::serde_as]
#[derive(Debug, serde::Serialize, serde::Deserialize, bincode::Encode, bincode::Decode)]
#[serde(rename_all = "camelCase")]
pub struct Runnable {
    pub command: Template,

    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<ArgValue>,

    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[serde_as(as = "serde_with::Map<_, _>")]
    pub env: Vec<(String, EnvValue)>,

    pub clear_env: bool,
}

#[derive(Debug, serde::Serialize, serde::Deserialize, bincode::Encode, bincode::Decode)]
#[serde(rename_all = "snake_case")]
#[serde(tag = "type")]
pub enum ArgValue {
    #[serde(rename_all = "camelCase")]
    Arg {
        value: Template,
    },
    Rest,
}

#[serde_with::serde_as]
#[derive(Debug, serde::Serialize, serde::Deserialize, bincode::Encode, bincode::Decode)]
#[serde(rename_all = "snake_case")]
#[serde(tag = "type")]
pub enum EnvValue {
    Clear,
    #[serde(rename_all = "camelCase")]
    Set {
        value: Template,
    },
    #[serde(rename_all = "camelCase")]
    Prepend {
        value: Template,
        #[serde_as(as = "TickEncoded")]
        separator: Vec<u8>,
    },
    #[serde(rename_all = "camelCase")]
    Append {
        value: Template,
        #[serde_as(as = "TickEncoded")]
        separator: Vec<u8>,
    },
}

#[serde_with::serde_as]
#[derive(Debug, serde::Serialize, serde::Deserialize, bincode::Encode, bincode::Decode)]
#[serde(rename_all = "camelCase")]
pub struct Template {
    components: Vec<TemplateComponent>,
}

impl Template {
    pub fn to_os_string(
        &self,
        program: &Path,
        resource_dirs: &[PathBuf],
    ) -> Result<std::ffi::OsString, RunnableTemplateError> {
        let mut os_string = std::ffi::OsString::new();

        for component in &self.components {
            match component {
                TemplateComponent::Literal { value } => {
                    let value = value.to_os_str()?;
                    os_string.push(value);
                }
                TemplateComponent::RelativePath { path } => {
                    let program_dir = program
                        .parent()
                        .ok_or(RunnableTemplateError::InvalidProgramPath)?;
                    let path = path.to_path()?;
                    let path = program_dir.join(path);
                    os_string.push(path);
                }
                TemplateComponent::Resource { resource } => {
                    let resource_subpath = resource.to_path()?;
                    let resource_path =
                        brioche_pack::find_in_resource_dirs(resource_dirs, resource_subpath)
                            .ok_or_else(|| {
                                let resource = bstr::BString::new(resource.clone());
                                RunnableTemplateError::ResourceNotFound { resource }
                            })?;
                    os_string.push(resource_path);
                }
            }
        }

        Ok(os_string)
    }
}

#[serde_with::serde_as]
#[derive(Debug, serde::Serialize, serde::Deserialize, bincode::Encode, bincode::Decode)]
#[serde(rename_all = "snake_case")]
#[serde(tag = "type")]
pub enum TemplateComponent {
    #[serde(rename_all = "camelCase")]
    Literal {
        #[serde_as(as = "TickEncoded")]
        value: Vec<u8>,
    },
    #[serde(rename_all = "camelCase")]
    RelativePath {
        #[serde_as(as = "TickEncoded")]
        path: Vec<u8>,
    },
    #[serde(rename_all = "camelCase")]
    Resource {
        #[serde_as(as = "TickEncoded")]
        resource: Vec<u8>,
    },
}

#[derive(Debug, thiserror::Error)]
pub enum RunnableTemplateError {
    #[error("invalid UTF-8 in runnable template: {0}")]
    Utf8Error(#[from] bstr::Utf8Error),
    #[error("invalid program path")]
    InvalidProgramPath,
    #[error(transparent)]
    PackResourceDirError(#[from] brioche_pack::PackResourceDirError),
    #[error("resource not found: {resource}")]
    ResourceNotFound { resource: bstr::BString },
}
