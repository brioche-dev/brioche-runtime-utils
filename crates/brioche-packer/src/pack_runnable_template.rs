use std::collections::HashMap;

use runnable_core::encoding::TickEncoded;

pub struct PackRunnableContext {
    pub resources: HashMap<String, Vec<u8>>,
}

impl PackRunnableContext {
    pub fn resource_paths(&self) -> Vec<Vec<u8>> {
        self.resources.values().cloned().collect()
    }
}

#[derive(Debug, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct PackRunnableTemplate {
    pub command: PackRunnableValueTemplate,
    pub args: Vec<PackRunnableArgValue>,
    pub env: HashMap<String, PackRunnableEnvValue>,
    pub clear_env: bool,
    pub source: Option<PackRunnableSource>,
}

impl PackRunnableTemplate {
    pub fn build(self, ctx: &PackRunnableContext) -> eyre::Result<runnable_core::Runnable> {
        let Self {
            command,
            args,
            env,
            clear_env,
            source,
        } = self;

        let command = command.build(ctx)?;
        let args = args
            .into_iter()
            .map(|arg| arg.build(ctx))
            .collect::<eyre::Result<_>>()?;
        let env = env
            .into_iter()
            .map(|(key, value)| {
                let value = value.build(ctx)?;
                Ok((key, value))
            })
            .collect::<eyre::Result<_>>()?;
        let source = source.map(|source| source.build(ctx)).transpose()?;
        Ok(runnable_core::Runnable {
            command,
            args,
            env,
            clear_env,
            source,
        })
    }
}

#[serde_with::serde_as]
#[derive(Debug, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
#[serde(tag = "type")]
pub enum PackRunnableValueTemplateComponent {
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
    Resource { name: String },
}

impl PackRunnableValueTemplateComponent {
    fn build(self, ctx: &PackRunnableContext) -> eyre::Result<runnable_core::TemplateComponent> {
        match self {
            PackRunnableValueTemplateComponent::Literal { value } => {
                Ok(runnable_core::TemplateComponent::Literal { value })
            }
            PackRunnableValueTemplateComponent::RelativePath { path } => {
                Ok(runnable_core::TemplateComponent::RelativePath { path })
            }
            PackRunnableValueTemplateComponent::Resource { name } => {
                let resource = ctx
                    .resources
                    .get(&name)
                    .ok_or_else(|| eyre::eyre!("resource not found: {name:?}"))?;
                Ok(runnable_core::TemplateComponent::Resource {
                    resource: resource.clone(),
                })
            }
        }
    }
}

#[derive(Debug, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct PackRunnableValueTemplate {
    pub components: Vec<PackRunnableValueTemplateComponent>,
}

impl PackRunnableValueTemplate {
    fn build(self, ctx: &PackRunnableContext) -> eyre::Result<runnable_core::Template> {
        let components = self
            .components
            .into_iter()
            .map(|component| component.build(ctx))
            .collect::<eyre::Result<_>>()?;
        Ok(runnable_core::Template { components })
    }
}

#[derive(Debug, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
#[serde(tag = "type")]
pub enum PackRunnableArgValue {
    Arg { value: PackRunnableValueTemplate },
    Rest,
}

impl PackRunnableArgValue {
    fn build(self, ctx: &PackRunnableContext) -> eyre::Result<runnable_core::ArgValue> {
        match self {
            PackRunnableArgValue::Arg { value } => {
                let value = value.build(ctx)?;
                Ok(runnable_core::ArgValue::Arg { value })
            }
            PackRunnableArgValue::Rest => Ok(runnable_core::ArgValue::Rest),
        }
    }
}

#[serde_with::serde_as]
#[derive(Debug, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
#[serde(tag = "type")]
pub enum PackRunnableEnvValue {
    Clear,
    Inherit,
    #[serde(rename_all = "camelCase")]
    Set {
        value: PackRunnableValueTemplate,
    },
    #[serde(rename_all = "camelCase")]
    Fallback {
        value: PackRunnableValueTemplate,
    },
    #[serde(rename_all = "camelCase")]
    Prepend {
        value: PackRunnableValueTemplate,
        #[serde_as(as = "TickEncoded")]
        separator: Vec<u8>,
    },
    #[serde(rename_all = "camelCase")]
    Append {
        value: PackRunnableValueTemplate,
        #[serde_as(as = "TickEncoded")]
        separator: Vec<u8>,
    },
}

impl PackRunnableEnvValue {
    fn build(self, ctx: &PackRunnableContext) -> eyre::Result<runnable_core::EnvValue> {
        match self {
            PackRunnableEnvValue::Clear => Ok(runnable_core::EnvValue::Clear),
            PackRunnableEnvValue::Inherit => Ok(runnable_core::EnvValue::Inherit),
            PackRunnableEnvValue::Set { value } => {
                let value = value.build(ctx)?;
                Ok(runnable_core::EnvValue::Set { value })
            }
            PackRunnableEnvValue::Fallback { value } => {
                let value = value.build(ctx)?;
                Ok(runnable_core::EnvValue::Fallback { value })
            }
            PackRunnableEnvValue::Prepend { value, separator } => {
                let value = value.build(ctx)?;
                Ok(runnable_core::EnvValue::Prepend { value, separator })
            }
            PackRunnableEnvValue::Append { value, separator } => {
                let value = value.build(ctx)?;
                Ok(runnable_core::EnvValue::Append { value, separator })
            }
        }
    }
}

#[derive(Debug, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct PackRunnableSource {
    pub path: PackRunnablePath,
}

impl PackRunnableSource {
    pub fn build(self, ctx: &PackRunnableContext) -> eyre::Result<runnable_core::RunnableSource> {
        let path = self.path.build(ctx)?;
        Ok(runnable_core::RunnableSource { path })
    }
}

#[serde_with::serde_as]
#[derive(Debug, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
#[serde(tag = "type")]
pub enum PackRunnablePath {
    #[serde(rename_all = "camelCase")]
    RelativePath { path: Vec<u8> },
    #[serde(rename_all = "camelCase")]
    Resource { name: String },
}

impl PackRunnablePath {
    fn build(self, ctx: &PackRunnableContext) -> eyre::Result<runnable_core::RunnablePath> {
        match self {
            PackRunnablePath::RelativePath { path } => {
                Ok(runnable_core::RunnablePath::RelativePath { path })
            }
            PackRunnablePath::Resource { name } => {
                let resource = ctx
                    .resources
                    .get(&name)
                    .ok_or_else(|| eyre::eyre!("resource not found: {name:?}"))?;
                Ok(runnable_core::RunnablePath::Resource {
                    resource: resource.clone(),
                })
            }
        }
    }
}
