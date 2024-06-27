use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
};

use bstr::ByteVec as _;
use runnable_core::encoding::TickEncoded;

pub struct AutowrapConfigTemplateContext {
    pub variables: HashMap<String, TemplateVariableValue>,
    pub resource_dir: PathBuf,
}

impl AutowrapConfigTemplateContext {
    fn get(&self, variable: &TemplateVariable) -> eyre::Result<&TemplateVariableValue> {
        self.variables
            .get(&variable.variable)
            .ok_or_else(|| eyre::eyre!("variable not set: {:?}", variable.variable))
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AutowrapConfigTemplate {
    #[serde(default)]
    paths: Vec<TemplatePath>,

    #[serde(default)]
    globs: Vec<String>,

    #[serde(default)]
    quiet: bool,

    #[serde(default)]
    link_dependencies: Vec<TemplatePath>,

    #[serde(default)]
    self_dependency: bool,

    dynamic_binary: Option<DynamicBinaryConfigTemplate>,

    shared_library: Option<SharedLibraryConfigTemplate>,

    script: Option<ScriptConfigTemplate>,

    rewrap: Option<RewrapConfigTemplate>,
}

impl AutowrapConfigTemplate {
    pub fn build(
        self,
        ctx: &AutowrapConfigTemplateContext,
        recipe_path: PathBuf,
    ) -> eyre::Result<super::AutowrapConfig> {
        let Self {
            paths,
            globs,
            quiet,
            link_dependencies,
            self_dependency,
            dynamic_binary,
            shared_library,
            script,
            rewrap,
        } = self;

        let paths = paths
            .into_iter()
            .map(|path| path.build(ctx))
            .collect::<eyre::Result<_>>()?;
        let link_dependencies = link_dependencies
            .into_iter()
            .map(|path| path.build(ctx))
            .collect::<eyre::Result<_>>()?;
        let dynamic_binary = dynamic_binary.map(|opts| opts.build(ctx)).transpose()?;
        let shared_library = shared_library.map(|opts| opts.build());
        let script = script.map(|opts| opts.build(ctx)).transpose()?;
        let rewrap = rewrap.map(|opts| opts.build());

        Ok(super::AutowrapConfig {
            recipe_path,
            paths,
            globs,
            quiet,
            link_dependencies,
            self_dependency,
            dynamic_binary,
            shared_library,
            script,
            rewrap,
        })
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
struct DynamicLinkingConfigTemplate {
    #[serde(default)]
    skip_libraries: HashSet<String>,

    #[serde(default)]
    extra_libraries: Vec<String>,

    #[serde(default)]
    skip_unknown_libraries: bool,
}

impl DynamicLinkingConfigTemplate {
    fn build(self) -> super::DynamicLinkingConfig {
        let Self {
            skip_libraries,
            extra_libraries,
            skip_unknown_libraries,
        } = self;

        super::DynamicLinkingConfig {
            skip_libraries,
            extra_libraries,
            skip_unknown_libraries,
        }
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct DynamicBinaryConfigTemplate {
    packed_executable: TemplatePath,

    #[serde(flatten)]
    dynamic_linking: DynamicLinkingConfigTemplate,
}

impl DynamicBinaryConfigTemplate {
    fn build(
        self,
        ctx: &AutowrapConfigTemplateContext,
    ) -> eyre::Result<super::DynamicBinaryConfig> {
        let Self {
            packed_executable,
            dynamic_linking,
        } = self;

        let packed_executable = packed_executable.build(ctx)?;
        let dynamic_linking = dynamic_linking.build();

        Ok(super::DynamicBinaryConfig {
            packed_executable,
            dynamic_linking,
        })
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SharedLibraryConfigTemplate {
    #[serde(flatten)]
    dynamic_linking: DynamicLinkingConfigTemplate,
}

impl SharedLibraryConfigTemplate {
    fn build(self) -> super::SharedLibraryConfig {
        let Self { dynamic_linking } = self;

        let dynamic_linking = dynamic_linking.build();

        super::SharedLibraryConfig { dynamic_linking }
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ScriptConfigTemplate {
    packed_executable: TemplatePath,

    #[serde(default)]
    env: HashMap<String, EnvValueTemplate>,

    #[serde(default)]
    clear_env: bool,
}

impl ScriptConfigTemplate {
    fn build(self, ctx: &AutowrapConfigTemplateContext) -> eyre::Result<super::ScriptConfig> {
        let Self {
            packed_executable,
            env,
            clear_env,
        } = self;

        let packed_executable = packed_executable.build(ctx)?;
        let env = env
            .into_iter()
            .map(|(env_var, value)| {
                let value = value.build(ctx, &env_var)?;
                eyre::Ok((env_var, value))
            })
            .collect::<eyre::Result<_>>()?;

        Ok(super::ScriptConfig {
            packed_executable,
            env,
            clear_env,
        })
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct RewrapConfigTemplate {}

impl RewrapConfigTemplate {
    fn build(self) -> super::RewrapConfig {
        let Self {} = self;
        super::RewrapConfig {}
    }
}

#[serde_with::serde_as]
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
#[serde(tag = "type")]
enum EnvValueTemplate {
    Clear,
    Inherit,
    #[serde(rename_all = "camelCase")]
    Set {
        value: EnvValueTemplateValue,
    },
    #[serde(rename_all = "camelCase")]
    Fallback {
        value: EnvValueTemplateValue,
    },
    #[serde(rename_all = "camelCase")]
    Prepend {
        value: EnvValueTemplateValue,
        #[serde_as(as = "TickEncoded")]
        separator: Vec<u8>,
    },
    #[serde(rename_all = "camelCase")]
    Append {
        value: EnvValueTemplateValue,
        #[serde_as(as = "TickEncoded")]
        separator: Vec<u8>,
    },
}

impl EnvValueTemplate {
    fn build(
        self,
        ctx: &AutowrapConfigTemplateContext,
        env_var: &str,
    ) -> eyre::Result<runnable_core::EnvValue> {
        match self {
            Self::Clear => Ok(runnable_core::EnvValue::Clear),
            Self::Inherit => Ok(runnable_core::EnvValue::Inherit),
            Self::Set { value } => {
                let value = value.build(ctx, env_var)?;
                Ok(runnable_core::EnvValue::Set { value })
            }
            Self::Fallback { value } => {
                let value = value.build(ctx, env_var)?;
                Ok(runnable_core::EnvValue::Fallback { value })
            }
            Self::Prepend { value, separator } => {
                let value = value.build(ctx, env_var)?;
                Ok(runnable_core::EnvValue::Prepend { value, separator })
            }
            Self::Append { value, separator } => {
                let value = value.build(ctx, env_var)?;
                Ok(runnable_core::EnvValue::Append { value, separator })
            }
        }
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
struct EnvValueTemplateValue {
    components: Vec<EnvValueTemplateValueComponent>,
}

impl EnvValueTemplateValue {
    fn build(
        self,
        ctx: &AutowrapConfigTemplateContext,
        env_var: &str,
    ) -> eyre::Result<runnable_core::Template> {
        let components = self
            .components
            .into_iter()
            .map(|component| component.build(ctx, env_var))
            .collect::<eyre::Result<_>>()?;

        Ok(runnable_core::Template { components })
    }
}

#[serde_with::serde_as]
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
#[serde(tag = "type")]
enum EnvValueTemplateValueComponent {
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
    Variable(TemplateVariable),
}

impl EnvValueTemplateValueComponent {
    fn build(
        self,
        ctx: &AutowrapConfigTemplateContext,
        env_var: &str,
    ) -> eyre::Result<runnable_core::TemplateComponent> {
        match self {
            Self::Literal { value } => Ok(runnable_core::TemplateComponent::Literal { value }),
            Self::RelativePath { path } => {
                Ok(runnable_core::TemplateComponent::RelativePath { path })
            }
            Self::Resource { resource } => {
                Ok(runnable_core::TemplateComponent::Resource { resource })
            }
            Self::Variable(variable) => {
                let value = ctx.get(&variable)?;
                match value {
                    TemplateVariableValue::Path(path) => {
                        let resource = brioche_resources::add_named_resource_directory(
                            &ctx.resource_dir,
                            path,
                            env_var,
                        )?;
                        let resource = <Vec<u8>>::from_path_buf(resource)
                            .map_err(|_| eyre::eyre!("invalid path"))?;
                        Ok(runnable_core::TemplateComponent::Resource { resource })
                    }
                }
            }
        }
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(untagged)]
enum TemplatePath {
    Path(PathBuf),
    Variable(TemplateVariable),
}

impl TemplatePath {
    fn build(self, ctx: &AutowrapConfigTemplateContext) -> eyre::Result<PathBuf> {
        match self {
            Self::Path(path) => Ok(path),
            Self::Variable(variable) => {
                let value = ctx.get(&variable)?;
                match value {
                    TemplateVariableValue::Path(path) => Ok(path.clone()),
                }
            }
        }
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TemplateVariable {
    variable: String,
}

#[derive(Debug, Clone)]
pub enum TemplateVariableValue {
    Path(PathBuf),
}
