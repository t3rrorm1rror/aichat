use crate::{
    config::{Message, SharedConfig},
    repl::{ReplyStreamHandler, SharedAbortSignal},
    utils::{init_tokio_runtime, prompt_input_integer, prompt_input_string, tokenize, PromptKind},
};

use anyhow::{Context, Result};
use async_trait::async_trait;
use reqwest::{Client as ReqwestClient, ClientBuilder, Proxy};
use serde::Deserialize;
use serde_json::{json, Value};
use std::{env, time::Duration};
use tokio::time::sleep;

use super::{openai::OpenAIConfig, ClientConfig};

#[macro_export]
macro_rules! register_client {
    (
        $(($module:ident, $name:literal, $config_key:ident, $config:ident, $client:ident),)+
    ) => {
        $(
            mod $module;
        )+
        $(
            use self::$module::$config;
        )+

        #[derive(Debug, Clone, serde::Deserialize)]
        #[serde(tag = "type")]
        pub enum ClientConfig {
            $(
                #[serde(rename = $name)]
                $config_key($config),
            )+
            #[serde(other)]
            Unknown,
        }


        $(
            #[derive(Debug)]
            pub struct $client {
                global_config: $crate::config::SharedConfig,
                config: $config,
                model_info: $crate::config::ModelInfo,
            }

            impl $client {
                pub const NAME: &str = $name;

                pub fn init(global_config: $crate::config::SharedConfig) -> Option<Box<dyn Client>> {
                    let model_info = global_config.read().model_info.clone();
                    let config = {
                        if let ClientConfig::$config_key(c) = &global_config.read().clients[model_info.index] {
                            c.clone()
                        } else {
                            return None;
                        }
                    };
                    Some(Box::new(Self {
                        global_config,
                        config,
                        model_info,
                    }))
                }

                pub fn name(local_config: &$config) -> &str {
                    local_config.name.as_deref().unwrap_or(Self::NAME)
                }
            }

        )+

        pub fn init_client(config: $crate::config::SharedConfig) -> anyhow::Result<Box<dyn Client>> {
                None
                $(.or_else(|| $client::init(config.clone())))+
                .ok_or_else(|| {
                    let model_info = config.read().model_info.clone();
                    anyhow::anyhow!(
                        "Unknown client {} at config.clients[{}]",
                        &model_info.client,
                        &model_info.index
                    )
                })
        }

        pub fn list_client_types() -> Vec<&'static str> {
            vec![$($client::NAME,)+]
        }

        pub fn create_client_config(client: &str) -> anyhow::Result<serde_json::Value> {
            $(
                if client == $client::NAME {
                    return create_config(&$client::PROMPTS, $client::NAME)
                }
            )+
            anyhow::bail!("Unknown client {}", client)
        }

        pub fn all_models(config: &$crate::config::Config) -> Vec<$crate::config::ModelInfo> {
            config
                .clients
                .iter()
                .enumerate()
                .flat_map(|(i, v)| match v {
                    $(ClientConfig::$config_key(c) => $client::list_models(c, i),)+
                    ClientConfig::Unknown => vec![],
                })
                .collect()
        }

    };
}

#[macro_export]
macro_rules! openai_compatible_client {
    ($client:ident) => {
        #[async_trait]
        impl $crate::client::Client for $crate::client::$client {
            fn config(
                &self,
            ) -> (
                &$crate::config::SharedConfig,
                &Option<$crate::client::ExtraConfig>,
            ) {
                (&self.global_config, &self.config.extra)
            }

            async fn send_message_inner(
                &self,
                client: &reqwest::Client,
                data: $crate::client::SendData,
            ) -> anyhow::Result<String> {
                let builder = self.request_builder(client, data)?;
                $crate::client::openai::openai_send_message(builder).await
            }

            async fn send_message_streaming_inner(
                &self,
                client: &reqwest::Client,
                handler: &mut $crate::repl::ReplyStreamHandler,
                data: $crate::client::SendData,
            ) -> Result<()> {
                let builder = self.request_builder(client, data)?;
                $crate::client::openai::openai_send_message_streaming(builder, handler).await
            }
        }
    };
}

#[macro_export]
macro_rules! config_get_fn {
    ($field_name:ident, $fn_name:ident) => {
        fn $fn_name(&self) -> anyhow::Result<String> {
            let api_key = self.config.$field_name.clone();
            api_key
                .or_else(|| {
                    let env_prefix = Self::name(&self.config);
                    let env_name =
                        format!("{}_{}", env_prefix, stringify!($field_name)).to_ascii_uppercase();
                    std::env::var(&env_name).ok()
                })
                .ok_or_else(|| anyhow::anyhow!("Miss {}", stringify!($field_name)))
        }
    };
}

#[async_trait]
pub trait Client {
    fn config(&self) -> (&SharedConfig, &Option<ExtraConfig>);

    fn build_client(&self) -> Result<ReqwestClient> {
        let mut builder = ReqwestClient::builder();
        let options = self.config().1;
        let timeout = options
            .as_ref()
            .and_then(|v| v.connect_timeout)
            .unwrap_or(10);
        let proxy = options.as_ref().and_then(|v| v.proxy.clone());
        builder = set_proxy(builder, &proxy)?;
        let client = builder
            .connect_timeout(Duration::from_secs(timeout))
            .build()
            .with_context(|| "Failed to build client")?;
        Ok(client)
    }

    fn send_message(&self, content: &str) -> Result<String> {
        init_tokio_runtime()?.block_on(async {
            let global_config = self.config().0;
            if global_config.read().dry_run {
                let content = global_config.read().echo_messages(content);
                return Ok(content);
            }
            let client = self.build_client()?;
            let data = global_config.read().prepare_send_data(content, false)?;
            self.send_message_inner(&client, data)
                .await
                .with_context(|| "Failed to get answer")
        })
    }

    fn send_message_streaming(
        &self,
        content: &str,
        handler: &mut ReplyStreamHandler,
    ) -> Result<()> {
        async fn watch_abort(abort: SharedAbortSignal) {
            loop {
                if abort.aborted() {
                    break;
                }
                sleep(Duration::from_millis(100)).await;
            }
        }
        let abort = handler.get_abort();
        init_tokio_runtime()?.block_on(async {
            tokio::select! {
                ret = async {
                    let global_config = self.config().0;
                    if global_config.read().dry_run {
                        let content = global_config.read().echo_messages(content);
                        let tokens = tokenize(&content);
                        for token in tokens {
                            tokio::time::sleep(Duration::from_millis(25)).await;
                            handler.text(&token)?;
                        }
                        return Ok(());
                    }
                    let client = self.build_client()?;
                    let data = global_config.read().prepare_send_data(content, true)?;
                    self.send_message_streaming_inner(&client, handler, data).await
                } => {
                    handler.done()?;
                    ret.with_context(|| "Failed to get answer")
                }
                _ = watch_abort(abort.clone()) => {
                    handler.done()?;
                    Ok(())
                 },
                _ =  tokio::signal::ctrl_c() => {
                    abort.set_ctrlc();
                    Ok(())
                }
            }
        })
    }

    async fn send_message_inner(&self, client: &ReqwestClient, data: SendData) -> Result<String>;

    async fn send_message_streaming_inner(
        &self,
        client: &ReqwestClient,
        handler: &mut ReplyStreamHandler,
        data: SendData,
    ) -> Result<()>;
}

impl Default for ClientConfig {
    fn default() -> Self {
        Self::OpenAI(OpenAIConfig::default())
    }
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct ExtraConfig {
    pub proxy: Option<String>,
    pub connect_timeout: Option<u64>,
}

#[derive(Debug)]
pub struct SendData {
    pub messages: Vec<Message>,
    pub temperature: Option<f64>,
    pub stream: bool,
}

pub type PromptType<'a> = (&'a str, &'a str, bool, PromptKind);

pub fn create_config(list: &[PromptType], client: &str) -> Result<Value> {
    let mut config = json!({
        "type": client,
    });
    for (path, desc, required, kind) in list {
        match kind {
            PromptKind::String => {
                let value = prompt_input_string(desc, *required)?;
                set_config_value(&mut config, path, kind, &value);
            }
            PromptKind::Integer => {
                let value = prompt_input_integer(desc, *required)?;
                set_config_value(&mut config, path, kind, &value);
            }
        }
    }

    let clients = json!(vec![config]);
    Ok(clients)
}

fn set_config_value(json: &mut Value, path: &str, kind: &PromptKind, value: &str) {
    let segs: Vec<&str> = path.split('.').collect();
    match segs.as_slice() {
        [name] => json[name] = to_json(kind, value),
        [scope, name] => match scope.split_once('[') {
            None => {
                if json.get(scope).is_none() {
                    let mut obj = json!({});
                    obj[name] = to_json(kind, value);
                    json[scope] = obj;
                } else {
                    json[scope][name] = to_json(kind, value);
                }
            }
            Some((scope, _)) => {
                if json.get(scope).is_none() {
                    let mut obj = json!({});
                    obj[name] = to_json(kind, value);
                    json[scope] = json!([obj]);
                } else {
                    json[scope][0][name] = to_json(kind, value);
                }
            }
        },
        _ => {}
    }
}

fn to_json(kind: &PromptKind, value: &str) -> Value {
    if value.is_empty() {
        return Value::Null;
    }
    match kind {
        PromptKind::String => value.into(),
        PromptKind::Integer => match value.parse::<i32>() {
            Ok(value) => value.into(),
            Err(_) => value.into(),
        },
    }
}

fn set_proxy(builder: ClientBuilder, proxy: &Option<String>) -> Result<ClientBuilder> {
    let proxy = if let Some(proxy) = proxy {
        if proxy.is_empty() || proxy == "false" || proxy == "-" {
            return Ok(builder);
        }
        proxy.clone()
    } else if let Ok(proxy) = env::var("HTTPS_PROXY").or_else(|_| env::var("ALL_PROXY")) {
        proxy
    } else {
        return Ok(builder);
    };
    let builder =
        builder.proxy(Proxy::all(&proxy).with_context(|| format!("Invalid proxy `{proxy}`"))?);
    Ok(builder)
}