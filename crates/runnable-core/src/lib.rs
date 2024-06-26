use std::path::{Path, PathBuf};

use bstr::{ByteSlice as _, ByteVec as _};
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
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, bincode::Encode, bincode::Decode)]
#[serde(rename_all = "snake_case")]
#[serde(tag = "type")]
pub enum EnvValue {
    Clear,
    Inherit,
    #[serde(rename_all = "camelCase")]
    Set {
        value: Template,
    },
    #[serde(rename_all = "camelCase")]
    Fallback {
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

impl EnvValue {
    pub fn fallback(&mut self, fallback_value: Template) {
        match self {
            EnvValue::Clear => {
                *self = EnvValue::Set {
                    value: fallback_value,
                };
            }
            EnvValue::Inherit => {
                *self = EnvValue::Fallback {
                    value: fallback_value,
                };
            }
            EnvValue::Set { value } => {
                value.fallback(fallback_value);
            }
            EnvValue::Fallback { value } => {
                value.fallback(fallback_value);
            }
            EnvValue::Prepend { .. } | EnvValue::Append { .. } => {}
        }
    }

    pub fn prepend(
        &mut self,
        prepend_value: Template,
        separator: &[u8],
    ) -> Result<(), RunnableTemplateError> {
        match self {
            EnvValue::Clear => {
                *self = EnvValue::Set {
                    value: prepend_value,
                };
                Ok(())
            }
            EnvValue::Inherit => {
                *self = EnvValue::Prepend {
                    value: prepend_value,
                    separator: separator.to_vec(),
                };
                Ok(())
            }
            EnvValue::Set { value } => {
                value.prepend(prepend_value, separator);
                Ok(())
            }
            EnvValue::Fallback { value } => {
                let mut value = std::mem::take(value);
                value.prepend(prepend_value, separator);
                *self = EnvValue::Prepend {
                    value,
                    separator: separator.to_vec(),
                };
                Ok(())
            }
            EnvValue::Prepend {
                value,
                separator: _,
            } => {
                value.prepend(prepend_value, separator);
                Ok(())
            }
            EnvValue::Append { .. } => Err(RunnableTemplateError::PrependAndAppend),
        }
    }

    pub fn append(
        &mut self,
        append_value: Template,
        separator: &[u8],
    ) -> Result<(), RunnableTemplateError> {
        match self {
            EnvValue::Clear => {
                *self = EnvValue::Set {
                    value: append_value,
                };
                Ok(())
            }
            EnvValue::Inherit => {
                *self = EnvValue::Append {
                    value: append_value,
                    separator: separator.to_vec(),
                };
                Ok(())
            }
            EnvValue::Set { value } => {
                value.append(append_value, separator);
                Ok(())
            }
            EnvValue::Fallback { value } => {
                let mut value = std::mem::take(value);
                value.append(append_value, separator);
                *self = EnvValue::Append {
                    value,
                    separator: separator.to_vec(),
                };
                Ok(())
            }
            EnvValue::Prepend { .. } => Err(RunnableTemplateError::PrependAndAppend),
            EnvValue::Append {
                value,
                separator: _,
            } => {
                value.append(append_value, separator);
                Ok(())
            }
        }
    }
}

#[serde_with::serde_as]
#[derive(
    Debug, Clone, Default, serde::Serialize, serde::Deserialize, bincode::Encode, bincode::Decode,
)]
#[serde(rename_all = "camelCase")]
pub struct Template {
    pub components: Vec<TemplateComponent>,
}

impl Template {
    pub fn from_literal(value: Vec<u8>) -> Self {
        if value.is_empty() {
            Self::default()
        } else {
            Self {
                components: vec![TemplateComponent::Literal { value }],
            }
        }
    }

    pub fn from_resource_path(resource_path: PathBuf) -> Result<Self, RunnableTemplateError> {
        let resource = Vec::<u8>::from_path_buf(resource_path)
            .map_err(|_| RunnableTemplateError::PathError)?;
        Ok(Self {
            components: vec![TemplateComponent::Resource { resource }],
        })
    }

    pub fn from_relative_path(path: PathBuf) -> Result<Self, RunnableTemplateError> {
        let path = Vec::<u8>::from_path_buf(path).map_err(|_| RunnableTemplateError::PathError)?;
        Ok(Self {
            components: vec![TemplateComponent::RelativePath { path }],
        })
    }

    pub fn is_empty(&self) -> bool {
        self.components.iter().all(|component| component.is_empty())
    }

    pub fn prepend(&mut self, prepend: Template, separator: &[u8]) {
        let current_components = std::mem::take(&mut self.components);

        self.components = prepend.components;
        self.append_literal(separator);
        self.components.extend(current_components);
    }

    pub fn append(&mut self, append: Template, separator: &[u8]) {
        self.append_literal(separator);
        self.components.extend(append.components);
    }

    pub fn fallback(&mut self, fallback: Template) {
        if self.is_empty() {
            *self = fallback;
        }
    }

    pub fn append_literal(&mut self, literal: &[u8]) {
        if literal.is_empty() {
            return;
        }

        if let Some(TemplateComponent::Literal { value }) = self.components.last_mut() {
            value.extend_from_slice(literal.as_ref());
        } else {
            self.components.push(TemplateComponent::Literal {
                value: literal.to_vec(),
            });
        }
    }

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
                        brioche_resources::find_in_resource_dirs(resource_dirs, resource_subpath)
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
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, bincode::Encode, bincode::Decode)]
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

impl TemplateComponent {
    fn is_empty(&self) -> bool {
        match self {
            Self::Literal { value } => value.is_empty(),
            Self::RelativePath { .. } | Self::Resource { .. } => false,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum RunnableTemplateError {
    #[error("invalid UTF-8 in runnable template: {0}")]
    Utf8Error(#[from] bstr::Utf8Error),
    #[error("invalid path in runnable template")]
    PathError,
    #[error("invalid program path")]
    InvalidProgramPath,
    #[error(transparent)]
    PackResourceDirError(#[from] brioche_resources::PackResourceDirError),
    #[error("resource not found: {resource}")]
    ResourceNotFound { resource: bstr::BString },
    #[error("tried prepending and appending to env var")]
    PrependAndAppend,
}
