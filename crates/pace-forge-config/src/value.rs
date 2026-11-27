//! 解析后的值与取值来源。

use std::collections::HashMap;

/// 解析后的单键值。键的 kind 由注册表保证，取值时按 kind 走对应入口。
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    /// 字符串（地址、机型代号等）。
    Str(String),
    /// 整数（端口、天数、次数）。
    Int(i64),
    /// 浮点数（比例、公里、分钟）。
    Float(f64),
    /// 布尔开关。
    Bool(bool),
}

impl Value {
    // The registry fixes each key's kind, so a mismatch is a programming error.
    #[expect(clippy::panic, reason = "kind is guaranteed by the registry")]
    pub(crate) fn as_str(&self) -> &str {
        match self {
            Value::Str(s) => s,
            other => panic!("配置键的 kind 不是字符串，却是 {other:?}"),
        }
    }
    // The registry fixes each key's kind, so a mismatch is a programming error.
    #[expect(clippy::panic, reason = "kind is guaranteed by the registry")]
    pub(crate) fn as_int(&self) -> i64 {
        match self {
            Value::Int(v) => *v,
            other => panic!("配置键的 kind 不是整数，却是 {other:?}"),
        }
    }
    // The registry fixes each key's kind, so a mismatch is a programming error.
    #[expect(clippy::panic, reason = "kind is guaranteed by the registry")]
    pub(crate) fn as_float(&self) -> f64 {
        match self {
            Value::Float(v) => *v,
            other => panic!("配置键的 kind 不是浮点数，却是 {other:?}"),
        }
    }
    // The registry fixes each key's kind, so a mismatch is a programming error.
    #[expect(clippy::panic, reason = "kind is guaranteed by the registry")]
    pub(crate) fn as_bool(&self) -> bool {
        match self {
            Value::Bool(v) => *v,
            other => panic!("配置键的 kind 不是布尔，却是 {other:?}"),
        }
    }
}

/// 一个键的取值来自哪一层。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// 没有任何地方设置，取注册表里的默认值。
    Default,
    /// 来自 `config.toml`。
    Toml,
    /// 来自 `PACE_*` 环境变量（优先级最高）。
    Env,
}

impl Source {
    /// 给人看的来源名，用在「非默认开关」那行提示里。
    pub fn label(self) -> &'static str {
        match self {
            Source::Default => "默认",
            Source::Toml => "config.toml",
            Source::Env => "env",
        }
    }
}

/// 键 → 来源。随 [`Config`](crate::Config) 一起加载，供启动摘要和排障显示。
pub type Sources = HashMap<&'static str, Source>;
